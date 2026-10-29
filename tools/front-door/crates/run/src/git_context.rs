//! Bounded host-side history for lanes whose tools cannot execute Git.
use crate::{Result, RunError, artifacts::seal};
use provenant_confinement::transport;
use provenant_os::{
    exec::{CapturedStream, ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::Directory,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

const CONTEXT_LIMIT: usize = 1024 * 1024;

pub fn continuation_history(cwd: &Path, base: &str) -> Result<String> {
    let git = provenant_os::git::git_executable().ok_or(RunError::Validation("Git unavailable"))?;
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
    // Bootstrap only the metadata paths with the hardened environment. Pin
    // both thereafter, including for ref resolution in linked worktrees.
    for (key, flag) in [
        ("GIT_DIR", "--absolute-git-dir"),
        ("GIT_COMMON_DIR", "--git-common-dir"),
    ] {
        let (out, timed_out) = read_git_with_env(
            executor,
            git,
            cwd,
            &["rev-parse", "--path-format=absolute", flag],
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
        env.push((key.into(), path));
    }
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
    let entries = transport::parse_listing(&listing.bytes).map_err(execution_error)?;
    let roots = lane_roots(cwd, env);
    transport::check_environment(transport_env, cwd, &roots).map_err(execution_error)?;
    transport::check_listing(&entries, cwd, &roots).map_err(execution_error)
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
    log_timed_out: bool,
    diff_timed_out: bool,
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
        let (diff, diff_timed_out) = read_git_with_env(
            executor,
            git,
            cwd,
            &[
                "diff",
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
            ],
            CONTEXT_LIMIT,
            &env,
        )?;
        Ok(Self {
            base,
            head,
            log,
            diff,
            log_timed_out,
            diff_timed_out,
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
        let metadata = serde_json::to_vec(&serde_json::json!({
            "base": self.base,
            "head": self.head,
            "max_bytes_per_file": CONTEXT_LIMIT,
            "log_truncated": self.log.truncated,
            "diff_truncated": self.diff.truncated,
            "timed_out": self.log_timed_out || self.diff_timed_out,
            "log_timed_out": self.log_timed_out,
            "diff_timed_out": self.diff_timed_out,
        }))
        .map_err(execution_error)?;
        seal(directory, "git-context.json", &metadata, 4096)?;
        Ok(format!(
            "Host-written Git context for {}..{}: log at {}; diff at {}; \
             bounds, truncation and timeouts at {}. Read these files for the review boundary; \
             they cover committed changes only.\n",
            self.base,
            self.head,
            scratch.join("git-log.txt").display(),
            scratch.join("git-diff.patch").display(),
            scratch.join("git-context.json").display(),
        ))
    }
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
            assert_eq!(
                &options.argv[1..4],
                ["--no-pager", "-c", "core.fsmonitor=false"]
            );
            assert!(options.stop_on_output_limit);
            let env = options.env.ok_or(ExecError::Busy)?;
            for (key, value) in environment() {
                assert!(env.contains(&(key, value)));
            }
            if !options.argv.contains(&"--absolute-git-dir".into())
                && !options.argv.contains(&"--git-common-dir".into())
            {
                for key in ["GIT_DIR", "GIT_COMMON_DIR"] {
                    assert!(env.contains(&(key.into(), "/fixture/repo/.git".into())));
                }
            }
            assert!(options.timeout <= Duration::from_secs(10));
            let mut calls = self.0.lock().map_err(|_| ExecError::Busy)?;
            calls.push(options.argv.to_vec());
            let bytes = if options.argv.contains(&"--absolute-git-dir".into())
                || options.argv.contains(&"--git-common-dir".into())
            {
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
    #[allow(clippy::disallowed_methods)]
    fn capture_uses_requested_head_instead_of_checkout() -> TestResult {
        use std::process::Command;
        let temp = tempfile::tempdir()?;
        let git = |args: &[&str]| -> TestResult {
            assert!(
                Command::new("git")
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
                Command::new("git")
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
        assert_eq!(executor.0.lock().map_err(|_| "calls poisoned")?.len(), 8);
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
            refused.to_string().contains("where lanes can write"),
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
}
