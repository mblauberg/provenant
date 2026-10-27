//! Bounded host-side history for lanes whose tools cannot execute Git.
use crate::{Result, RunError, artifacts::seal};
use provenant_os::{
    exec::{CapturedStream, ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::Directory,
};
use std::{ffi::OsString, path::Path, time::Duration};

const CONTEXT_LIMIT: usize = 1024 * 1024;

pub fn continuation_history(cwd: &Path) -> Result<String> {
    let git = provenant_os::git::git_executable().ok_or(RunError::Validation("Git unavailable"))?;
    let executor = SystemExecutor::new(1).map_err(execution_error)?;
    let env = repository_environment(&executor, &git, cwd)?;
    let (out, timed_out) = read_git_with_env(
        &executor,
        &git,
        cwd,
        &[
            "log",
            "--max-count=50",
            "--format=%h %s",
            "main..HEAD",
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

pub struct GitContext {
    base: String,
    head: String,
    log: CapturedStream,
    diff: CapturedStream,
    log_timed_out: bool,
    diff_timed_out: bool,
}
impl GitContext {
    /// Capture on the host once, before preparing any provider candidate.
    pub fn capture(git: &Path, cwd: &Path, base: &str) -> Result<Self> {
        let executor = SystemExecutor::new(1).map_err(execution_error)?;
        Self::capture_with(&executor, git, cwd, base)
    }

    fn capture_with(executor: &dyn Executor, git: &Path, cwd: &Path, base: &str) -> Result<Self> {
        let env = repository_environment(executor, git, cwd)?;
        let base = resolve(executor, git, cwd, base, &env)?;
        let head = resolve(executor, git, cwd, "HEAD", &env)?;
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
        assert_eq!(executor.0.lock().map_err(|_| "calls poisoned")?.len(), 6);
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
            Err(RunError::UnsafePath)
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
