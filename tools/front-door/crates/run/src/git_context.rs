//! Bounded host-side history for lanes whose tools cannot execute Git.
use crate::{Result, RunError, artifacts::seal};
use provenant_confinement::transport;
use provenant_os::{
    exec::{CapturedStream, ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::Directory,
};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

const CONTEXT_LIMIT: usize = 1024 * 1024;
/// Per-file slices written beside the whole patch. A larger change keeps the
/// whole patch and the stat, and the index counts the files left without a slice.
const SLICE_LIMIT: usize = 400;
const SLICE_DIRECTORY: &str = "git-diff-slices";
const SLICE_INDEX: &str = "index.txt";
const SLICE_INDEX_LIMIT: u64 = 128 * 1024;

pub fn continuation_history(cwd: &Path, base: &str) -> Result<String> {
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    let executor = SystemExecutor::new(1).map_err(execution_error)?;
    continuation_history_with(&executor, &git, cwd, base)
}

fn continuation_history_with(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
    base: &str,
) -> Result<String> {
    let env = repository_environment(executor, git, cwd)?;
    let base = resolve(executor, git, cwd, base, &env)?;
    let range = format!("{base}..HEAD");
    let (out, timed_out) = read_git_with_env(
        executor,
        git,
        cwd,
        &[
            "log",
            "--no-show-signature",
            "--max-count=50",
            "--format=%h %s",
            &range,
            "--",
        ],
        16_384,
        &env,
    )?;
    let mut text = String::from_utf8_lossy(&out.bytes).into_owned();
    if timed_out || out.truncated {
        text.push_str("\n[history truncated or timed out]\n");
    }
    Ok(text)
}
// Keep command-scoped overrides above repository/worktree configuration, as
// confinement's utility reader does. Protocol denial also covers Git < 2.44,
// which does not honour GIT_NO_LAZY_FETCH.
const GIT_CONFIG_OVERRIDES: &[(&str, &str)] = &[
    ("protocol.allow", "never"),
    ("credential.helper", ""),
    ("credential.interactive", "false"),
    ("core.hooksPath", "/dev/null"),
    ("core.attributesFile", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("log.showSignature", "false"),
];

fn environment() -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_ATTR_NOSYSTEM", "1"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_GRAFT_FILE", "/dev/null"),
        ("GIT_ALLOW_PROTOCOL", ""),
        ("LC_ALL", "C"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    env.push((
        "GIT_CONFIG_COUNT".into(),
        GIT_CONFIG_OVERRIDES.len().to_string().into(),
    ));
    for (index, (key, value)) in GIT_CONFIG_OVERRIDES.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{index}").into(), (*key).into()));
        env.push((format!("GIT_CONFIG_VALUE_{index}").into(), (*value).into()));
    }
    env
}

fn repository_environment(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
) -> Result<Vec<(OsString, OsString)>> {
    use std::os::unix::ffi::OsStringExt;
    let mut env = environment();
    // Resolve the common directory without trusting inherited redirects, then
    // pin the private directory for subsequent Git operations.
    let common = provenant_os::git::repository_path_with(
        executor,
        git,
        cwd,
        provenant_os::git::RepositoryPath::CommonDir,
    )
    .map_err(execution_error)?;
    let (out, timed_out) = read_git_with_env(
        executor,
        git,
        cwd,
        &["rev-parse", "--path-format=absolute", "--absolute-git-dir"],
        4096,
        &env,
    )?;
    if timed_out || out.truncated || out.bytes.last() != Some(&b'\n') {
        return Err(execution_error("Git metadata path read was incomplete"));
    }
    let path = OsString::from_vec(out.bytes[..out.bytes.len() - 1].to_vec());
    if !Path::new(&path).is_absolute() {
        return Err(execution_error("Git metadata path was not absolute"));
    }
    env.push(("GIT_DIR".into(), path));
    env.push(("GIT_COMMON_DIR".into(), common.into_os_string()));
    env.push(("GIT_WORK_TREE".into(), cwd.as_os_str().to_owned()));
    Ok(env)
}

fn execution_error(error: impl std::fmt::Display) -> RunError {
    RunError::Io(std::io::Error::other(error.to_string()))
}

#[cfg(test)]
fn read_git(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
    args: &[&str],
    limit: usize,
) -> Result<CapturedStream> {
    read_git_with_env(executor, git, cwd, args, limit, &environment()).map(|(out, _)| out)
}

fn read_git_with_env(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
    args: &[&str],
    limit: usize,
    env: &[(OsString, OsString)],
) -> Result<(CapturedStream, bool)> {
    let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
        .chain(
            ["--no-pager", "-c", "core.fsmonitor=false"]
                .into_iter()
                .chain(args.iter().copied())
                .map(OsString::from),
        )
        .collect();
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(10));
    options.env = Some(env);
    options.stdout = StreamPolicy::Capture { limit };
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    options.stop_on_output_limit = true;
    let mut out = executor.execute(options).map_err(execution_error)?;
    if out.cancelled
        || (!out.timed_out
            && (out.drain_truncated
                || out.stdout.read_failed
                || out.stderr.read_failed
                || out.stderr.truncated
                || (out.termination != Termination::Exit(0)
                    && !(out.overflow_stopped && out.stdout.truncated))))
    {
        return Err(execution_error("Git context read failed or timed out"));
    }
    // A deadline bounds the host's work, not lane admission. Preserve whatever
    // was captured and tell the reviewer explicitly that coverage is partial.
    out.stdout.truncated |= out.drain_truncated || out.stdout.read_failed;
    Ok((out.stdout, out.timed_out))
}

/// The `-c` arguments every advertisement shares with delivery and the
/// confinement utility: no hooks, no fsmonitor, only the usual transports.
fn transport_args() -> Vec<String> {
    transport::CONFIG
        .iter()
        .flat_map(|(key, value)| ["-c".to_owned(), format!("{key}={value}")])
        .collect()
}

/// The host environment for talking to a remote, bound to this repository.
fn transport_environment(env: &[(OsString, OsString)]) -> Vec<(OsString, OsString)> {
    let bound = env.iter().filter(|(key, _)| {
        matches!(
            key.to_str(),
            Some("GIT_DIR" | "GIT_WORK_TREE" | "GIT_COMMON_DIR")
        )
    });
    transport::environment(std::env::vars_os(), bound.cloned())
}

/// Where a lane can write: its linked worktree, the project's worktree and
/// scratch areas, and the run state directory when one is set.
fn lane_roots(cwd: &Path, env: &[(OsString, OsString)]) -> transport::LaneRoots {
    let get = |name: &str| {
        env.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| PathBuf::from(value))
    };
    let run_state = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).map(PathBuf::from);
    let extra = transport::LaneRoots::new(run_state);
    match (get("GIT_DIR"), get("GIT_COMMON_DIR")) {
        (Some(private), Some(common)) => {
            transport::LaneRoots::for_repository(cwd, &private, &common, &extra)
        }
        _ => transport::LaneRoots::project(cwd),
    }
}

/// Refuse an advertisement whose effective configuration is read from a
/// lane-writable file, or names a transport program in a lane-writable
/// location. `Ok` says nothing about whether the remote is reachable.
fn check_transport(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
    env: &[(OsString, OsString)],
    transport_env: &[(OsString, OsString)],
) -> Result<()> {
    let hardening = transport_args();
    let args: Vec<&str> = hardening
        .iter()
        .map(String::as_str)
        .chain(transport::LISTING_ARGS)
        .collect();
    let (listing, timed_out) =
        read_git_with_env(executor, git, cwd, &args, CONTEXT_LIMIT, transport_env)?;
    if timed_out || listing.truncated {
        return Err(execution_error("Git configuration listing was incomplete"));
    }
    let refusal = |error: transport::TransportError| RunError::GitTransport {
        kind: (&error).into(),
        reason: error.to_string(),
    };
    let entries = transport::parse_listing(&listing.bytes).map_err(refusal)?;
    let roots = lane_roots(cwd, env);
    transport::check_environment(transport_env, cwd, &roots).map_err(refusal)?;
    transport::check_listing(&entries, cwd, &roots).map_err(refusal)
}

/// Look up the exact object ID the remote advertises for a tracking ref named
/// by `root`. A bare remote name or its `HEAD` stands for the advertised default.
fn advertised_oid(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
    root: &str,
    remote_name: &str,
    env: &[(OsString, OsString)],
    transport_env: &[(OsString, OsString)],
) -> Option<String> {
    let (resolved_remote, branch) = remote_name.split_once('/')?;
    // Symbolic resolution dereferences HEAD. Identify an explicitly requested
    // remote HEAD from the configured remote names, rather than trusting the
    // local symref's target to name that remote.
    let (remotes, timed_out) =
        read_git_with_env(executor, git, cwd, &["remote"], 16384, env).ok()?;
    if timed_out || remotes.truncated {
        return None;
    }
    let remotes = std::str::from_utf8(&remotes.bytes).ok()?;
    let requested_default = remotes.lines().find(|remote| {
        root == *remote
            || root == format!("{remote}/HEAD")
            || root == format!("remotes/{remote}/HEAD")
            || root == format!("refs/remotes/{remote}/HEAD")
    });
    let remote = requested_default.unwrap_or(resolved_remote);
    let default = requested_default.is_some() || branch == "HEAD";
    let reference = format!("refs/heads/{branch}");
    let hardening = transport_args();
    let hardening: Vec<&str> = hardening.iter().map(String::as_str).collect();
    let tail = if default {
        vec![
            "ls-remote",
            "--symref",
            "--exit-code",
            "--",
            remote,
            "HEAD",
            "refs/heads/*",
        ]
    } else {
        vec![
            "ls-remote",
            "--exit-code",
            "--heads",
            "--",
            remote,
            reference.as_str(),
        ]
    };
    let args: Vec<&str> = hardening.into_iter().chain(tail).collect();
    let (advertised, timed_out) =
        read_git_with_env(executor, git, cwd, &args, CONTEXT_LIMIT, transport_env).ok()?;
    if timed_out || advertised.truncated {
        return None;
    }
    let advertised = std::str::from_utf8(&advertised.bytes).ok()?;
    let mut default_branch = None;
    let mut heads = Vec::new();
    for line in advertised.lines() {
        let (value, name) = line.split_once('\t')?;
        if name == "HEAD" && value.starts_with("ref: ") {
            let name = value.strip_prefix("ref: refs/heads/")?;
            if default_branch.replace(name).is_some() {
                return None;
            }
        } else if let Some(name) = name.strip_prefix("refs/heads/") {
            provenant_core::GitOid::parse(value).ok()?;
            heads.push((name, value));
        }
    }
    let branch = if default { default_branch? } else { branch };
    let mut matches = heads.iter().filter(|(name, _)| *name == branch);
    let (_, oid) = matches.next()?;
    matches.next().is_none().then(|| (*oid).to_owned())
}

fn resolve(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
    base: &str,
    env: &[(OsString, OsString)],
) -> Result<String> {
    let invalid = || RunError::InvalidBase(base.to_owned());
    if base.is_empty() || base.len() > 1024 || base.contains('\0') {
        return Err(invalid());
    }
    // A tracking ref can alias a differently spelled ref on the host's
    // filesystem. Replace its root with the exact advertised object ID and
    // apply any revision operators to that.
    let (root, suffix) = provenant_core::split_revision_root(base);
    let (symbolic, timed_out) = read_git_with_env(
        executor,
        git,
        cwd,
        &[
            "rev-parse",
            "--symbolic-full-name",
            "--verify",
            "--end-of-options",
            root,
        ],
        4096,
        env,
    )
    .map_err(|_| invalid())?;
    if timed_out || symbolic.truncated {
        return Err(invalid());
    }
    let symbolic = std::str::from_utf8(&symbolic.bytes)
        .map_err(|_| invalid())?
        .trim();
    let verified;
    let base = if let Some(remote_name) = symbolic.strip_prefix("refs/remotes/") {
        // The remote answers with host credentials and programs, so first
        // refuse any that a lane could have supplied.
        let transport_env = transport_environment(env);
        check_transport(executor, git, cwd, env, &transport_env)?;
        let oid = advertised_oid(executor, git, cwd, root, remote_name, env, &transport_env)
            .ok_or_else(invalid)?;
        verified = format!("{oid}{suffix}");
        verified.as_str()
    } else {
        base
    };
    let (out, timed_out) = read_git_with_env(
        executor,
        git,
        cwd,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
        256,
        env,
    )
    .map_err(|_| invalid())?;
    let text = std::str::from_utf8(&out.bytes).map_err(|_| invalid())?;
    provenant_core::GitOid::parse(text.trim()).map_err(|_| invalid())?;
    if timed_out || out.truncated {
        return Err(invalid());
    }
    Ok(text.trim().to_owned())
}

/// Validate the base before admission; persist the resolved commit in the
/// immutable task so a moving ref cannot change a queued lane's boundary.
pub fn resolve_git_base(git: &Path, cwd: &Path, base: &str) -> Result<String> {
    let executor = SystemExecutor::new(1).map_err(execution_error)?;
    let env = repository_environment(&executor, git, cwd)
        .map_err(|_| RunError::InvalidBase(base.to_owned()))?;
    resolve(&executor, git, cwd, base, &env)
}

/// Resolve the merge base against the locally recorded default origin branch.
pub fn review_merge_base(git: &Path, cwd: &Path, head: &str) -> Result<String> {
    let executor = SystemExecutor::new(1).map_err(execution_error)?;
    let env = repository_environment(&executor, git, cwd)?;
    let default = resolve(&executor, git, cwd, "refs/remotes/origin/HEAD", &env).map_err(|_| {
        RunError::Validation("cannot resolve origin's default branch; supply --base")
    })?;
    let (out, timed_out) = read_git_with_env(
        &executor,
        git,
        cwd,
        &["merge-base", head, &default],
        256,
        &env,
    )
    .map_err(|_| RunError::Validation("cannot find review merge base; supply --base"))?;
    let value = std::str::from_utf8(&out.bytes)
        .map_err(execution_error)?
        .trim();
    if timed_out || out.truncated || provenant_core::GitOid::parse(value).is_err() {
        return Err(RunError::Validation(
            "cannot find review merge base; supply --base",
        ));
    }
    Ok(value.to_owned())
}

pub struct GitContext {
    base: String,
    head: String,
    log: CapturedStream,
    diff: CapturedStream,
    stat: CapturedStream,
    log_timed_out: bool,
    diff_timed_out: bool,
    stat_timed_out: bool,
}
impl GitContext {
    /// Capture a specified review head on the host.
    pub fn capture_at(git: &Path, cwd: &Path, base: &str, head: &str) -> Result<Self> {
        let executor = SystemExecutor::new(1).map_err(execution_error)?;
        Self::capture_with_head(&executor, git, cwd, base, head)
    }
    /// Capture on the host once, before preparing any provider candidate.
    pub fn capture(git: &Path, cwd: &Path, base: &str) -> Result<Self> {
        let executor = SystemExecutor::new(1).map_err(execution_error)?;
        Self::capture_with(&executor, git, cwd, base)
    }

    fn capture_with(executor: &dyn Executor, git: &Path, cwd: &Path, base: &str) -> Result<Self> {
        Self::capture_with_head(executor, git, cwd, base, "HEAD")
    }

    fn capture_with_head(
        executor: &dyn Executor,
        git: &Path,
        cwd: &Path,
        base: &str,
        head: &str,
    ) -> Result<Self> {
        let env = repository_environment(executor, git, cwd)?;
        let base = resolve(executor, git, cwd, base, &env)?;
        let head = resolve(executor, git, cwd, head, &env)?;
        let range = format!("{base}..{head}");
        let (log, log_timed_out) = read_git_with_env(
            executor,
            git,
            cwd,
            &[
                "log",
                "--no-mailmap",
                "--no-ext-diff",
                "--no-textconv",
                "--no-show-signature",
                "--no-decorate",
                "--format=medium",
                "--no-color",
                "--no-patch",
                &range,
                "--",
            ],
            CONTEXT_LIMIT,
            &env,
        )?;
        let diff_args = |format: &'static str| {
            [
                "diff",
                format,
                "--text",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--submodule=short",
                "--ignore-submodules=none",
                "--no-color",
                "--no-relative",
                &range,
                "--",
            ]
        };
        let (diff, diff_timed_out) = read_git_with_env(
            executor,
            git,
            cwd,
            &diff_args("--patch"),
            CONTEXT_LIMIT,
            &env,
        )?;
        // Wide enough that long paths are not shortened with `...`.
        let (stat, stat_timed_out) = read_git_with_env(
            executor,
            git,
            cwd,
            &diff_args("--stat=240,200,1000"),
            CONTEXT_LIMIT,
            &env,
        )?;
        Ok(Self {
            base,
            head,
            log,
            diff,
            stat,
            log_timed_out,
            diff_timed_out,
            stat_timed_out,
        })
    }

    /// Files share the candidate's existing scratch grant and terminal cleanup.
    pub fn write_to(&self, directory: &Directory, scratch: &Path) -> Result<String> {
        seal(
            directory,
            "git-log.txt",
            &self.log.bytes,
            CONTEXT_LIMIT as u64,
        )?;
        seal(
            directory,
            "git-diff.patch",
            &self.diff.bytes,
            CONTEXT_LIMIT as u64,
        )?;
        seal(
            directory,
            "git-diff.stat",
            &self.stat.bytes,
            CONTEXT_LIMIT as u64,
        )?;
        let slices = self.write_slices(directory)?;
        let diff_cut = self.diff.truncated || self.diff_timed_out;
        let metadata = serde_json::to_vec(&serde_json::json!({
            "base": self.base,
            "head": self.head,
            "max_bytes_per_file": CONTEXT_LIMIT,
            "log_truncated": self.log.truncated,
            "diff_truncated": self.diff.truncated,
            "stat_truncated": self.stat.truncated,
            "timed_out": self.log_timed_out || self.diff_timed_out || self.stat_timed_out,
            "log_timed_out": self.log_timed_out,
            "diff_timed_out": self.diff_timed_out,
            "stat_timed_out": self.stat_timed_out,
            "slices": slices.written,
            "slices_omitted": slices.omitted,
            "slices_complete": !diff_cut && slices.omitted == 0,
        }))
        .map_err(execution_error)?;
        seal(directory, "git-context.json", &metadata, 4096)?;
        let slice_directory = scratch.join(SLICE_DIRECTORY);
        let incomplete = if diff_cut || slices.omitted > 0 {
            self.incomplete_note(&slices.paths)
        } else {
            String::new()
        };
        Ok(format!(
            "Host-written Git context for {}..{}: log at {}; per-file diff stat at {}; \
             one patch slice per changed file in {} (index at {}); whole diff at {}; \
             bounds, truncation and timeouts at {}. The stat and slices are navigation aids, \
             not the scope of the review. Start from the stat and read the slice for each file \
             you examine, then read the surrounding code in the repository: callers and \
             callees, existing utilities the change should reuse, and whether it fits the \
             architecture. Open the whole diff only when a question spans files. These files \
             cover committed changes only.{}\n",
            self.base,
            self.head,
            scratch.join("git-log.txt").display(),
            scratch.join("git-diff.stat").display(),
            slice_directory.display(),
            slice_directory.join(SLICE_INDEX).display(),
            scratch.join("git-diff.patch").display(),
            scratch.join("git-context.json").display(),
            incomplete,
        ))
    }

    /// Say the slices are incomplete and name the changed files from the stat
    /// that have no slice, so a reviewer reads those from the whole patch or
    /// the checkout.
    fn incomplete_note(&self, sliced: &BTreeSet<Vec<u8>>) -> String {
        const LISTED: usize = 50;
        let missing: Vec<String> = stat_paths(&self.stat.bytes)
            .into_iter()
            .filter(|path| !sliced.contains(path))
            .map(|path| String::from_utf8_lossy(&path).into_owned())
            .collect();
        let mut note = String::from(
            " The slices are incomplete: the whole patch was cut by its bounds or the slice \
             limit.",
        );
        if missing.is_empty() {
            note.push_str(
                " Read any file you cannot find a slice for from the whole patch or the checkout.",
            );
        } else {
            note.push_str(
                " These changed files have no slice; read them from the whole patch or the \
                 checkout: ",
            );
            note.push_str(
                &missing
                    .iter()
                    .take(LISTED)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            if missing.len() > LISTED {
                note.push_str(&format!(
                    ", and {} more in the stat",
                    missing.len() - LISTED
                ));
            }
            note.push('.');
        }
        if self.stat.truncated || self.stat_timed_out {
            note.push_str(" The stat is itself incomplete, so other files may be missing from it.");
        }
        note
    }

    /// Split the whole patch into one file per changed path, with an index
    /// naming each slice, so a reviewer reads the files it needs and not the
    /// whole patch again.
    fn write_slices(&self, directory: &Directory) -> Result<SliceCounts> {
        let name = std::ffi::OsStr::new(SLICE_DIRECTORY);
        match directory.mkdir(name, 0o700) {
            Ok(()) => {}
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        let slices = directory.open_verified_directory(name)?;
        let files = split_patch(&self.diff.bytes);
        let mut index = String::from(
            "# One slice per changed file, in patch order: slice, bytes, path.\n\
             # The stat lists every changed file. A file missing here was cut from the whole\n\
             # patch by its bounds, or left without a slice by the slice limit.\n",
        );
        if self.diff.truncated || self.diff_timed_out {
            index.push_str("# The whole patch is incomplete: the last slice may be partial.\n");
        }
        let mut written = 0;
        let mut paths = BTreeSet::new();
        for (number, file) in files.iter().take(SLICE_LIMIT).enumerate() {
            let name = format!("{:04}-{}.patch", number + 1, slice_label(&file.path));
            seal(&slices, &name, file.bytes, CONTEXT_LIMIT as u64)?;
            index.push_str(&format!(
                "{name}\t{}\t{}\n",
                file.bytes.len(),
                String::from_utf8_lossy(&file.path)
            ));
            paths.insert(file.path.clone());
            written += 1;
        }
        let omitted = files.len() - written;
        if omitted > 0 {
            index.push_str(&format!(
                "# {omitted} more changed files have no slice; read them from the whole patch.\n"
            ));
        }
        seal(&slices, SLICE_INDEX, index.as_bytes(), SLICE_INDEX_LIMIT)?;
        Ok(SliceCounts {
            written,
            omitted,
            paths,
        })
    }
}

struct SliceCounts {
    written: usize,
    omitted: usize,
    /// The path of each written slice, as the patch header printed it.
    paths: BTreeSet<Vec<u8>>,
}

/// The changed paths a `git diff --stat` lists: each line up to its ` | `.
fn stat_paths(stat: &[u8]) -> Vec<Vec<u8>> {
    stat.split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let at = line.windows(3).rposition(|window| window == b" | ")?;
            let path = line[..at].trim_ascii();
            (!path.is_empty()).then(|| path.to_vec())
        })
        .collect()
}

struct FileSlice<'a> {
    /// The path as Git printed it in the header, quoted when Git quoted it.
    path: Vec<u8>,
    bytes: &'a [u8],
}

/// Cut a patch at its `diff --git` headers. Every body line starts with a
/// space, `+`, `-`, `@` or a backslash, so a line beginning `diff --git ` is
/// always a header.
fn split_patch(patch: &[u8]) -> Vec<FileSlice<'_>> {
    const HEADER: &[u8] = b"diff --git ";
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in patch.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(HEADER) {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(at, start)| {
            let end = starts.get(at + 1).copied().unwrap_or(patch.len());
            let bytes = &patch[*start..end];
            let header = bytes
                .split(|byte| *byte == b'\n')
                .next()
                .unwrap_or_default();
            FileSlice {
                path: header_path(&header[HEADER.len()..]),
                bytes,
            }
        })
        .collect()
}

/// The path of a `diff --git a/P b/P` header. Renames are off, so both sides
/// name the same path.
fn header_path(rest: &[u8]) -> Vec<u8> {
    if let Some(quoted) = rest.strip_prefix(b"\"a/") {
        let mut at = 0;
        while let Some(byte) = quoted.get(at) {
            at += if *byte == b'\\' { 2 } else { 1 };
            if *byte == b'"' {
                break;
            }
        }
        return [b"\"", &quoted[..at.min(quoted.len())]].concat();
    }
    // `a/P b/P`: the two sides are equally long.
    let side = rest.len() / 2;
    if rest.len() % 2 == 1 && rest.get(side) == Some(&b' ') && rest[..side].starts_with(b"a/") {
        return rest[2..side].to_vec();
    }
    rest.to_vec()
}

/// A short, filesystem-safe tail of the path for the slice name.
fn slice_label(path: &[u8]) -> String {
    let text: Vec<char> = String::from_utf8_lossy(path)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    text[text.len().saturating_sub(80)..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_os::exec::ExecError;
    use provenant_os::exec::ExecOutcome;
    use std::sync::Mutex;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
    const OID: &str = "1111111111111111111111111111111111111111";

    struct InspectGit(Mutex<Vec<Vec<OsString>>>);
    impl Executor for InspectGit {
        fn execute(&self, options: ExecOptions<'_>) -> std::result::Result<ExecOutcome, ExecError> {
            let common_query = options.argv.contains(&"--git-common-dir".into());
            if !common_query {
                assert_eq!(
                    &options.argv[1..4],
                    ["--no-pager", "-c", "core.fsmonitor=false"]
                );
            }
            assert!(options.stop_on_output_limit);
            let env = options.env.ok_or(ExecError::Busy)?;
            if !common_query {
                for (key, value) in environment() {
                    assert!(env.contains(&(key, value)));
                }
            }
            if !options.argv.contains(&"--absolute-git-dir".into())
                && !options.argv.contains(&"--git-common-dir".into())
            {
                assert!(env.contains(&("GIT_DIR".into(), "/fixture/repo/.git".into())));
                let common =
                    std::fs::canonicalize(std::env::temp_dir()).map_err(|_| ExecError::Busy)?;
                assert!(env.contains(&("GIT_COMMON_DIR".into(), common.into_os_string())));
            }
            assert!(options.timeout <= Duration::from_secs(10));
            let mut calls = self.0.lock().map_err(|_| ExecError::Busy)?;
            calls.push(options.argv.to_vec());
            let bytes = if common_query {
                use std::os::unix::ffi::OsStrExt;
                let mut bytes = std::env::temp_dir().as_os_str().as_bytes().to_vec();
                bytes.push(b'\n');
                bytes
            } else if options.argv.contains(&"--absolute-git-dir".into()) {
                b"/fixture/repo/.git\n".to_vec()
            } else if options.argv[4] == "rev-parse" {
                format!("{OID}\n").into_bytes()
            } else {
                assert!(options.argv.contains(&"--no-ext-diff".into()));
                assert!(options.argv.contains(&"--no-textconv".into()));
                if options.argv[4] == "log" {
                    assert!(options.argv.contains(&"--no-mailmap".into()));
                } else {
                    assert!(options.argv.contains(&"--text".into()));
                    assert!(options.argv.contains(&"--no-renames".into()));
                }
                b"context".to_vec()
            };
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream {
                    bytes,
                    ..Default::default()
                },
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 1,
            })
        }
    }

    struct DrainFixture(SystemExecutor);
    impl Executor for DrainFixture {
        fn execute(
            &self,
            mut options: ExecOptions<'_>,
        ) -> std::result::Result<ExecOutcome, ExecError> {
            // The contributor lane cannot signal children. Drain this finite
            // fixture while retaining the production capture limits.
            options.stop_on_output_limit = false;
            self.0.execute(options)
        }
    }

    #[test]
    fn capture_uses_requested_head_instead_of_checkout() -> TestResult {
        let temp = tempfile::tempdir()?;
        let git = |args: &[&str]| -> TestResult {
            assert!(
                provenant_os::fixture::git_command()
                    .current_dir(temp.path())
                    .args(args)
                    .status()?
                    .success()
            );
            Ok(())
        };
        git(&["init", "-q"])?;
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ])?;
        let oid = || -> std::result::Result<String, Box<dyn std::error::Error>> {
            Ok(String::from_utf8(
                provenant_os::fixture::git_command()
                    .current_dir(temp.path())
                    .args(["rev-parse", "HEAD"])
                    .output()?
                    .stdout,
            )?
            .trim()
            .to_owned())
        };
        let base = oid()?;
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "commit",
            "--allow-empty",
            "-qm",
            "reviewed",
        ])?;
        let head = oid()?;
        git(&["checkout", "-q", "--detach", &base])?;
        let binary = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let context = GitContext::capture_at(&binary, temp.path(), &base, &head)?;
        assert_eq!(context.head, head);
        assert!(String::from_utf8_lossy(&context.log.bytes).contains("reviewed"));
        Ok(())
    }

    #[test]
    fn git_context_accepts_stdout_overflow_but_refuses_stderr_overflow() -> TestResult {
        struct Completed(ExecOutcome);
        impl Executor for Completed {
            fn execute(&self, _: ExecOptions<'_>) -> std::result::Result<ExecOutcome, ExecError> {
                Ok(self.0.clone())
            }
        }
        let mut out = ExecOutcome {
            termination: Termination::Exit(0),
            stdout: CapturedStream {
                bytes: b"partial".to_vec(),
                ..Default::default()
            },
            stderr: CapturedStream::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 1,
        };
        out.termination = Termination::Signal(15);
        out.overflow_stopped = true;
        out.stdout.truncated = true;
        let read = |out| {
            read_git(
                &Completed(out),
                Path::new("/fixture/git"),
                Path::new("/fixture"),
                &["diff"],
                128,
            )
        };
        assert!(read(out.clone())?.truncated);
        out.stderr.truncated = true;
        assert!(read(out.clone()).is_err());
        out.stdout.truncated = false;
        assert!(read(out).is_err());
        Ok(())
    }

    #[test]
    fn git_context_pins_hardening_flags_and_writes_prompt_paths() -> TestResult {
        let executor = InspectGit(Mutex::new(Vec::new()));
        let context = GitContext::capture_with(
            &executor,
            Path::new("/fixture/git"),
            Path::new("/fixture/repo"),
            "main",
        )?;
        let scratch = tempfile::tempdir()?;
        let prompt = context.write_to(&Directory::open_anchor(scratch.path())?, scratch.path())?;
        assert_eq!(
            std::fs::read(scratch.path().join("git-log.txt"))?,
            b"context"
        );
        assert_eq!(
            std::fs::read(scratch.path().join("git-diff.patch"))?,
            b"context"
        );
        assert!(prompt.contains(&scratch.path().join("git-log.txt").display().to_string()));
        assert!(prompt.contains(&format!("{OID}..{OID}")));
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(scratch.path().join("git-context.json"))?)?;
        assert_eq!(metadata["log_truncated"], false);
        assert_eq!(metadata["diff_truncated"], false);
        assert_eq!(executor.0.lock().map_err(|_| "calls poisoned")?.len(), 9);
        Ok(())
    }

    #[test]
    fn git_context_timeout_preserves_partial_files_and_records_metadata() -> TestResult {
        struct Timeout(InspectGit);
        impl Executor for Timeout {
            fn execute(
                &self,
                options: ExecOptions<'_>,
            ) -> std::result::Result<ExecOutcome, ExecError> {
                let context = options.argv[4] == "log" || options.argv[4] == "diff";
                let mut out = self.0.execute(options)?;
                if context {
                    out.termination = Termination::Signal(15);
                    out.timed_out = true;
                }
                Ok(out)
            }
        }
        let context = GitContext::capture_with(
            &Timeout(InspectGit(Mutex::new(Vec::new()))),
            Path::new("/fixture/git"),
            Path::new("/fixture/repo"),
            "main",
        )?;
        let scratch = tempfile::tempdir()?;
        context.write_to(&Directory::open_anchor(scratch.path())?, scratch.path())?;
        assert_eq!(
            std::fs::read(scratch.path().join("git-log.txt"))?,
            b"context"
        );
        assert_eq!(
            std::fs::read(scratch.path().join("git-diff.patch"))?,
            b"context"
        );
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(scratch.path().join("git-context.json"))?)?;
        assert_eq!(metadata["timed_out"], true);
        assert_eq!(metadata["log_timed_out"], true);
        assert_eq!(metadata["diff_timed_out"], true);
        Ok(())
    }

    #[test]
    fn git_context_writes_through_held_scratch_after_parent_redirection() -> TestResult {
        let context = GitContext::capture_with(
            &InspectGit(Mutex::new(Vec::new())),
            Path::new("/fixture/git"),
            Path::new("/fixture/repo"),
            "main",
        )?;
        let temp = tempfile::tempdir()?;
        let parent = temp.path().join("tmp");
        let scratch = parent.join("candidate-0");
        std::fs::create_dir_all(&scratch)?;
        let directory = Directory::open_anchor(&scratch)?;
        let redirected = temp.path().join("redirected");
        std::fs::create_dir_all(redirected.join("candidate-0"))?;
        let held_parent = temp.path().join("held-tmp");
        std::fs::rename(&parent, &held_parent)?;
        std::os::unix::fs::symlink(&redirected, &parent)?;
        context.write_to(&directory, &scratch)?;
        assert!(!redirected.join("candidate-0/git-log.txt").exists());
        assert_eq!(
            std::fs::read(held_parent.join("candidate-0/git-log.txt"))?,
            b"context"
        );
        Ok(())
    }

    #[test]
    fn git_context_refuses_a_staged_file_it_cannot_read_back() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let context = GitContext::capture_with(
            &InspectGit(Mutex::new(Vec::new())),
            Path::new("/fixture/git"),
            Path::new("/fixture/repo"),
            "main",
        )?;
        let scratch = tempfile::tempdir()?;
        let log = scratch.path().join("git-log.txt");
        std::fs::write(&log, b"context")?;
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644))?;
        let directory = Directory::open_anchor(scratch.path())?;
        assert!(matches!(
            context.write_to(&directory, scratch.path()),
            Err(RunError::UnsafePath { .. })
        ));
        Ok(())
    }

    fn git(executor: &SystemExecutor, binary: &Path, cwd: &Path, args: &[&str]) -> TestResult {
        read_git(executor, binary, cwd, args, 4096)?;
        Ok(())
    }

    #[test]
    fn git_context_ignores_worktree_binary_attributes_with_control() -> TestResult {
        let HistoryFixture {
            _temp,
            root,
            binary,
            executor,
            base,
        } = small_history()?;
        std::fs::write(root.join(".gitattributes"), "* -diff\n")?;
        let range = format!("{base}..HEAD");
        let control = read_git(&executor, &binary, &root, &["diff", &range], 4096)?;
        assert!(String::from_utf8_lossy(&control.bytes).contains("Binary files"));
        let context = GitContext::capture(&binary, &root, &base)?;
        assert!(String::from_utf8_lossy(&context.diff.bytes).contains("+changed line"));
        Ok(())
    }

    #[test]
    fn git_context_ignores_worktree_mailmap_with_control() -> TestResult {
        let HistoryFixture {
            _temp,
            root,
            binary,
            executor,
            base,
        } = small_history()?;
        std::fs::write(
            root.join(".mailmap"),
            "Rewritten <rewritten@example.test> Fixture <fixture@example.test>\n",
        )?;
        let range = format!("{base}..HEAD");
        let control = read_git(
            &executor,
            &binary,
            &root,
            &["log", "--format=medium", &range],
            4096,
        )?;
        assert!(String::from_utf8_lossy(&control.bytes).contains("Author: Rewritten"));
        let context = GitContext::capture(&binary, &root, &base)?;
        assert!(
            String::from_utf8_lossy(&context.log.bytes)
                .contains("Author: Fixture <fixture@example.test>")
        );
        Ok(())
    }

    struct HistoryFixture {
        _temp: tempfile::TempDir,
        root: std::path::PathBuf,
        binary: std::path::PathBuf,
        executor: SystemExecutor,
        base: String,
    }

    fn small_history() -> std::result::Result<HistoryFixture, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let binary = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let executor = SystemExecutor::new(1)?;
        git(&executor, &binary, &root, &["init", "--quiet"])?;
        for text in ["original line\n", "changed line\n"] {
            std::fs::write(root.join("tracked.txt"), text)?;
            git(&executor, &binary, &root, &["add", "tracked.txt"])?;
            git(
                &executor,
                &binary,
                &root,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.test",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ],
            )?;
        }
        let base = resolve_git_base(&binary, &root, "HEAD^")?;
        Ok(HistoryFixture {
            _temp: temp,
            root,
            binary,
            executor,
            base,
        })
    }

    #[test]
    fn remote_base_requires_exact_advertised_oid() -> TestResult {
        struct TestHome<'a> {
            executor: &'a SystemExecutor,
            home: &'a Path,
        }
        impl Executor for TestHome<'_> {
            fn execute(
                &self,
                options: ExecOptions<'_>,
            ) -> std::result::Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError>
            {
                let mut env = options.env.unwrap_or_default().to_vec();
                env.retain(|(key, _)| key != "HOME");
                env.push(("HOME".into(), self.home.as_os_str().to_owned()));
                let mut isolated = ExecOptions::new(options.argv, options.cwd, options.timeout);
                isolated.env = Some(&env);
                isolated.stdout = options.stdout;
                isolated.stderr = options.stderr;
                isolated.stop_on_output_limit = options.stop_on_output_limit;
                self.executor.execute(isolated)
            }
        }
        let fixture = small_history()?;
        let executor = TestHome {
            executor: &fixture.executor,
            home: &fixture.root,
        };
        let env = repository_environment(&executor, &fixture.binary, &fixture.root)?;
        let resolve_base =
            |base: &str| resolve(&executor, &fixture.binary, &fixture.root, base, &env);
        let run = |args: &[&str]| git(&fixture.executor, &fixture.binary, &fixture.root, args);
        run(&["branch", "-M", "main"])?;
        run(&[
            "remote",
            "add",
            "origin",
            fixture.root.to_str().ok_or("non-UTF8 root")?,
        ])?;
        run(&["update-ref", "refs/remotes/origin/main", &fixture.base])?;
        run(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ])?;
        run(&["config", "branch.main.remote", "origin"])?;
        run(&["config", "branch.main.merge", "refs/heads/main"])?;
        run(&["config", "push.default", "upstream"])?;
        let aliases = [
            "origin/main",
            "remotes/origin/main",
            "origin",
            "@{u}",
            "main@{upstream}",
            "@{push}",
            "refs/remotes/origin/HEAD",
        ];
        let head = resolve_git_base(&fixture.binary, &fixture.root, "HEAD")?;
        for base in aliases {
            assert_eq!(resolve_base(base)?, head, "{base}");
        }
        run(&["update-ref", "refs/remotes/origin/main", &head])?;
        for base in aliases {
            assert_eq!(resolve_base(base)?, head, "{base}");
        }
        // Operators apply to the verified root, not to the stale tracking ref
        // (whose own parent does not exist). Every spelling of a tracking root
        // takes the same suffixes: `HEAD` forms are followed to the advertised
        // default, and a compound suffix is applied to that.
        run(&["update-ref", "refs/remotes/origin/main", &fixture.base])?;
        for root in [
            "origin/main",
            "remotes/origin/main",
            "refs/remotes/origin/main",
            "origin/HEAD",
            "remotes/origin/HEAD",
            "refs/remotes/origin/HEAD",
            "origin",
            "@{u}",
            "main@{upstream}",
            "@{push}",
        ] {
            for (suffix, expected) in [
                ("^", fixture.base.as_str()),
                ("~1", fixture.base.as_str()),
                ("^{commit}", head.as_str()),
                ("~1^{commit}", fixture.base.as_str()),
                ("^0", head.as_str()),
            ] {
                assert_eq!(
                    resolve_base(&format!("{root}{suffix}"))?,
                    expected,
                    "{root}{suffix}"
                );
            }
        }

        // A tracking ref its remote cannot vouch for fails closed instead of
        // falling back to the local OID: no such remote is configured, and (on
        // a case-insensitive filesystem) a remote spelled in another case.
        run(&["update-ref", "refs/remotes/gone/main", &head])?;
        let mut unverifiable = vec![
            "gone/main",
            "gone/main~1",
            "refs/remotes/gone/main^{commit}",
        ];
        let probe = fixture.root.join("CaseProbe");
        std::fs::write(&probe, b"")?;
        if fixture.root.join("caseprobe").exists() {
            unverifiable.extend(["Origin/main", "Origin/main~1"]);
        }
        for base in unverifiable {
            assert!(
                matches!(resolve_base(base), Err(RunError::InvalidBase(named)) if named == base),
                "{base} must not fall back to the local tracking OID"
            );
        }

        // Configuration or programs a lane could have supplied are refused
        // before any advertisement; the user's own helpers elsewhere are not.
        let lane = fixture.root.join(".worktrees/lane-1/upload-pack");
        std::fs::create_dir_all(lane.parent().ok_or("no parent")?)?;
        std::fs::write(&lane, "#!/bin/sh\nexit 1\n")?;
        let lane = lane.to_str().ok_or("non-UTF8 path")?;
        run(&["config", "remote.origin.uploadpack", lane])?;
        let refused = resolve_base("origin/main")
            .err()
            .ok_or("lane helper must fail")?;
        assert!(
            matches!(
                refused,
                RunError::GitTransport {
                    kind: crate::TransportRefusal::LaneProgram,
                    ..
                }
            ),
            "{refused}"
        );
        run(&["config", "remote.origin.uploadpack", "git-upload-pack"])?;
        assert_eq!(resolve_base("origin/main")?, head);
        Ok(())
    }

    #[test]
    fn continuation_history_never_asks_git_to_verify_signatures() -> TestResult {
        struct Records<'a>(&'a SystemExecutor, Mutex<Vec<Vec<OsString>>>);
        impl Executor for Records<'_> {
            fn execute(
                &self,
                options: ExecOptions<'_>,
            ) -> std::result::Result<ExecOutcome, ExecError> {
                self.1
                    .lock()
                    .map_err(|_| ExecError::Busy)?
                    .push(options.argv.to_vec());
                self.0.execute(options)
            }
        }
        let fixture = small_history()?;
        let executor = Records(&fixture.executor, Mutex::new(Vec::new()));
        let history =
            continuation_history_with(&executor, &fixture.binary, &fixture.root, &fixture.base)?;
        assert!(history.contains("fixture"));
        let calls = executor.1.lock().map_err(|_| "calls poisoned")?;
        let log = calls
            .iter()
            .find(|argv| argv.contains(&"log".into()))
            .ok_or("no log call")?;
        assert!(log.contains(&"--no-show-signature".into()), "{log:?}");
        // The environment override covers every other host read as well.
        assert!(GIT_CONFIG_OVERRIDES.contains(&("log.showSignature", "false")));
        Ok(())
    }

    #[test]
    fn continuation_history_uses_the_recorded_base_without_main() -> TestResult {
        let fixture = small_history()?;
        git(
            &fixture.executor,
            &fixture.binary,
            &fixture.root,
            &["branch", "-M", "custom-base"],
        )?;
        let history = continuation_history(&fixture.root, &fixture.base)?;
        assert_eq!(history.lines().count(), 1, "{history}");
        assert!(history.contains("fixture"));
        assert!(continuation_history(&fixture.root, "main").is_err());
        Ok(())
    }

    #[test]
    fn git_context_bounds_real_history_and_refuses_bad_base_without_helpers() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir()?;
        let root = std::fs::canonicalize(scratch.path())?;
        let binary = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let executor = SystemExecutor::new(1)?;
        git(&executor, &binary, &root, &["init", "--quiet"])?;
        std::fs::write(root.join("large.txt"), "base\n")?;
        git(&executor, &binary, &root, &["add", "large.txt"])?;
        let commit = |message: &str| {
            git(
                &executor,
                &binary,
                &root,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.test",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    message,
                ],
            )
        };
        commit("base")?;
        let base = resolve_git_base(&binary, &root, "HEAD")?;
        std::fs::write(
            root.join("large.txt"),
            "changed line\n".repeat(CONTEXT_LIMIT / 4),
        )?;
        git(&executor, &binary, &root, &["add", "large.txt"])?;
        // Git reads the oversized message from a file, avoiding argv limits.
        std::fs::write(
            root.join("message"),
            "large log message\n".repeat(CONTEXT_LIMIT / 4),
        )?;
        git(
            &executor,
            &binary,
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-F",
                "message",
            ],
        )?;
        let helper = root.join("helper");
        std::fs::write(&helper, "#!/bin/sh\ntouch helper-ran\n")?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
        let helper_text = helper.to_str().ok_or("helper path")?;
        for key in [
            "core.fsmonitor",
            "diff.external",
            "diff.fixture.textconv",
            "gpg.program",
        ] {
            git(&executor, &binary, &root, &["config", key, helper_text])?;
        }
        git(
            &executor,
            &binary,
            &root,
            &["config", "log.showSignature", "true"],
        )?;
        std::fs::write(root.join(".gitattributes"), "*.txt diff=fixture\n")?;
        let output = root.join("context");
        std::fs::create_dir(&output)?;
        let context =
            GitContext::capture_with(&DrainFixture(executor.clone()), &binary, &root, &base)?;
        context.write_to(&Directory::open_anchor(&output)?, &output)?;
        for name in ["git-log.txt", "git-diff.patch"] {
            assert_eq!(
                std::fs::metadata(output.join(name))?.len(),
                CONTEXT_LIMIT as u64
            );
        }
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("git-context.json"))?)?;
        assert_eq!(metadata["log_truncated"], true);
        assert_eq!(metadata["diff_truncated"], true);
        assert!(!root.join("helper-ran").exists());
        assert!(
            matches!(resolve_git_base(&binary, &root, "absent-base"), Err(RunError::InvalidBase(base)) if base == "absent-base")
        );
        assert!(matches!(
            resolve_git_base(&binary, &root, "--help"),
            Err(RunError::InvalidBase(_))
        ));
        assert!(GitContext::capture(&binary, &root, "absent-base").is_err());
        Ok(())
    }

    fn context_with_diff(diff: &[u8], truncated: bool) -> GitContext {
        let captured = |bytes: &[u8], truncated| CapturedStream {
            bytes: bytes.to_vec(),
            truncated,
            ..Default::default()
        };
        GitContext {
            base: OID.into(),
            head: OID.into(),
            log: captured(b"log", false),
            diff: captured(diff, truncated),
            stat: captured(b" stat\n", false),
            log_timed_out: false,
            diff_timed_out: false,
            stat_timed_out: false,
        }
    }

    fn patch_for(path: &str, body: &str) -> String {
        format!(
            "diff --git a/{path} b/{path}\nindex 111..222 100644\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-{body}\n+{body}!\n"
        )
    }

    #[test]
    fn patch_headers_name_plain_quoted_and_odd_paths() {
        let plain = patch_for("src/a b.rs", "x");
        let quoted = "diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"\nindex 1..2\n";
        let tricky =
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-diff --git a/fake b/fake\n+kept\n";
        let patch = format!("{plain}{quoted}{tricky}");
        let files = split_patch(patch.as_bytes());
        let paths: Vec<_> = files
            .iter()
            .map(|file| String::from_utf8_lossy(&file.path).into_owned())
            .collect();
        assert_eq!(paths, ["src/a b.rs", "\"caf\\303\\251.txt\"", "x"]);
        let joined: Vec<u8> = files.iter().flat_map(|file| file.bytes.to_vec()).collect();
        assert_eq!(joined, patch.as_bytes());
        assert!(split_patch(b"").is_empty());
        assert_eq!(slice_label(b"src/a b.rs"), "src_a_b.rs");
        assert_eq!(slice_label("é".repeat(100).as_bytes()).chars().count(), 80);
    }

    #[test]
    fn slices_cover_every_file_and_the_index_names_them() -> TestResult {
        let patch = ["a.rs", "dir/b.rs", "c.md"]
            .map(|path| patch_for(path, path))
            .concat();
        let context = context_with_diff(patch.as_bytes(), false);
        let scratch = tempfile::tempdir()?;
        let prompt = context.write_to(&Directory::open_anchor(scratch.path())?, scratch.path())?;
        assert_eq!(
            std::fs::read(scratch.path().join("git-diff.stat"))?,
            b" stat\n"
        );
        let slices = scratch.path().join(SLICE_DIRECTORY);
        let index = std::fs::read_to_string(slices.join(SLICE_INDEX))?;
        let rows: Vec<_> = index
            .lines()
            .filter(|line| !line.starts_with('#'))
            .collect();
        assert_eq!(rows.len(), 3, "{index}");
        let mut joined = Vec::new();
        for (row, path) in rows.iter().zip(["a.rs", "dir/b.rs", "c.md"]) {
            let fields: Vec<_> = row.split('\t').collect();
            assert_eq!(fields[2], path);
            let bytes = std::fs::read(slices.join(fields[0]))?;
            assert_eq!(bytes.len().to_string(), fields[1]);
            assert!(
                String::from_utf8(bytes.clone())?.starts_with(&format!("diff --git a/{path} "))
            );
            joined.extend(bytes);
        }
        assert_eq!(joined, patch.as_bytes());
        assert!(rows[1].starts_with("0002-dir_b.rs.patch\t"), "{index}");
        for needle in [
            scratch.path().join("git-diff.stat"),
            slices.clone(),
            slices.join(SLICE_INDEX),
            scratch.path().join("git-diff.patch"),
        ] {
            assert!(prompt.contains(&needle.display().to_string()), "{prompt}");
        }
        assert!(prompt.contains("navigation aids"), "{prompt}");
        assert!(prompt.contains("surrounding code"), "{prompt}");
        assert!(!prompt.contains("incomplete"), "{prompt}");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(scratch.path().join("git-context.json"))?)?;
        assert_eq!(metadata["slices"], 3);
        assert_eq!(metadata["slices_omitted"], 0);
        assert_eq!(metadata["slices_complete"], true);
        assert_eq!(metadata["stat_truncated"], false);
        Ok(())
    }

    #[test]
    fn slices_stop_at_their_limit_and_say_so() -> TestResult {
        let patch: String = (0..SLICE_LIMIT + 5)
            .map(|n| patch_for(&format!("f{n}.txt"), "x"))
            .collect();
        let context = context_with_diff(patch.as_bytes(), false);
        let scratch = tempfile::tempdir()?;
        context.write_to(&Directory::open_anchor(scratch.path())?, scratch.path())?;
        let slices = scratch.path().join(SLICE_DIRECTORY);
        let index = std::fs::read_to_string(slices.join(SLICE_INDEX))?;
        assert!(
            index.contains("# 5 more changed files have no slice"),
            "{index}"
        );
        // The limit plus the index.
        assert_eq!(std::fs::read_dir(&slices)?.count(), SLICE_LIMIT + 1);
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(scratch.path().join("git-context.json"))?)?;
        assert_eq!(metadata["slices"], SLICE_LIMIT);
        assert_eq!(metadata["slices_omitted"], 5);
        assert_eq!(metadata["slices_complete"], false);
        Ok(())
    }

    #[test]
    fn an_incomplete_prompt_names_the_changed_files_without_a_slice() -> TestResult {
        let patch: String = (0..SLICE_LIMIT + 2)
            .map(|n| patch_for(&format!("f{n}.txt"), "x"))
            .collect();
        let mut context = context_with_diff(patch.as_bytes(), false);
        let stat: String = (0..SLICE_LIMIT + 2)
            .map(|n| format!(" f{n}.txt | 2 +-\n"))
            .chain([" 402 files changed, 402 insertions(+)\n".to_owned()])
            .collect();
        context.stat.bytes = stat.into_bytes();
        let scratch = tempfile::tempdir()?;
        let prompt = context.write_to(&Directory::open_anchor(scratch.path())?, scratch.path())?;
        assert!(prompt.contains("The slices are incomplete"), "{prompt}");
        let last = SLICE_LIMIT + 1;
        assert!(
            prompt.contains(&format!("f{}.txt, f{last}.txt.", SLICE_LIMIT)),
            "{prompt}"
        );
        assert!(!prompt.contains("f0.txt,"), "{prompt}");
        Ok(())
    }

    #[test]
    fn a_truncated_patch_marks_its_last_slice_partial() -> TestResult {
        let patch = patch_for("a.rs", "x");
        let context = context_with_diff(&patch.as_bytes()[..patch.len() - 6], true);
        let scratch = tempfile::tempdir()?;
        context.write_to(&Directory::open_anchor(scratch.path())?, scratch.path())?;
        let index =
            std::fs::read_to_string(scratch.path().join(SLICE_DIRECTORY).join(SLICE_INDEX))?;
        assert!(index.contains("the last slice may be partial"), "{index}");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(scratch.path().join("git-context.json"))?)?;
        assert_eq!(metadata["slices_complete"], false);
        Ok(())
    }

    #[test]
    fn real_git_stat_and_slices_match_the_whole_patch() -> TestResult {
        let fixture = small_history()?;
        std::fs::create_dir_all(fixture.root.join("deep/dir"))?;
        std::fs::write(fixture.root.join("deep/dir/new file.txt"), "new\n")?;
        std::fs::write(fixture.root.join("second.txt"), "second\n")?;
        git(
            &fixture.executor,
            &fixture.binary,
            &fixture.root,
            &["add", "."],
        )?;
        git(
            &fixture.executor,
            &fixture.binary,
            &fixture.root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "more files",
            ],
        )?;
        let context = GitContext::capture(&fixture.binary, &fixture.root, &fixture.base)?;
        let scratch = tempfile::tempdir()?;
        context.write_to(&Directory::open_anchor(scratch.path())?, scratch.path())?;
        let stat = std::fs::read_to_string(scratch.path().join("git-diff.stat"))?;
        for path in ["deep/dir/new file.txt", "second.txt"] {
            assert!(stat.contains(path), "{stat}");
        }
        assert!(stat.contains("3 files changed"), "{stat}");
        let slices = scratch.path().join(SLICE_DIRECTORY);
        let index = std::fs::read_to_string(slices.join(SLICE_INDEX))?;
        let mut joined = Vec::new();
        for row in index.lines().filter(|line| !line.starts_with('#')) {
            joined.extend(std::fs::read(
                slices.join(row.split('\t').next().unwrap_or("")),
            )?);
        }
        assert_eq!(
            joined,
            std::fs::read(scratch.path().join("git-diff.patch"))?
        );
        Ok(())
    }
}
