//! Bounded host-side history for lanes whose tools cannot execute Git.
use crate::{Result, RunError, artifacts::seal};
use provenant_os::{
    exec::{CapturedStream, ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::Directory,
};
use std::{ffi::OsString, path::Path, time::Duration};

const CONTEXT_LIMIT: usize = 1024 * 1024;
/// Per-file slices written beside the whole patch. A larger change keeps the
/// whole patch and the stat, and the index counts the files left without a slice.
const SLICE_LIMIT: usize = 400;
const SLICE_DIRECTORY: &str = "git-diff-slices";
const SLICE_INDEX: &str = "index.txt";
const SLICE_INDEX_LIMIT: u64 = 128 * 1024;

pub fn continuation_history(cwd: &Path, base: &str) -> Result<String> {
    let git = provenant_os::git::git_executable().ok_or(RunError::Validation("Git unavailable"))?;
    let executor = SystemExecutor::new(1).map_err(execution_error)?;
    let env = repository_environment(&executor, &git, cwd)?;
    let base = resolve(&executor, &git, cwd, base, &env)?;
    let range = format!("{base}..HEAD");
    let (out, timed_out) = read_git_with_env(
        &executor,
        &git,
        cwd,
        &["log", "--max-count=50", "--format=%h %s", &range, "--"],
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
        Ok(format!(
            "Host-written Git context for {}..{}: log at {}; per-file diff stat at {}; \
             one patch slice per changed file in {} (index at {}); whole diff at {}; \
             bounds, truncation and timeouts at {}. Read the stat first, then only the slices \
             for the files you review; open the whole diff only for a question that spans \
             files. They cover committed changes only.\n",
            self.base,
            self.head,
            scratch.join("git-log.txt").display(),
            scratch.join("git-diff.stat").display(),
            slice_directory.display(),
            slice_directory.join(SLICE_INDEX).display(),
            scratch.join("git-diff.patch").display(),
            scratch.join("git-context.json").display(),
        ))
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
        for (number, file) in files.iter().take(SLICE_LIMIT).enumerate() {
            let name = format!("{:04}-{}.patch", number + 1, slice_label(&file.path));
            seal(&slices, &name, file.bytes, CONTEXT_LIMIT as u64)?;
            index.push_str(&format!(
                "{name}\t{}\t{}\n",
                file.bytes.len(),
                String::from_utf8_lossy(&file.path)
            ));
            written += 1;
        }
        let omitted = files.len() - written;
        if omitted > 0 {
            index.push_str(&format!(
                "# {omitted} more changed files have no slice; read them from the whole patch.\n"
            ));
        }
        seal(&slices, SLICE_INDEX, index.as_bytes(), SLICE_INDEX_LIMIT)?;
        Ok(SliceCounts { written, omitted })
    }
}

struct SliceCounts {
    written: usize,
    omitted: usize,
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
        assert_eq!(executor.0.lock().map_err(|_| "calls poisoned")?.len(), 7);
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
        assert!(prompt.contains("Read the stat first"));
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
