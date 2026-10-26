//! Bounded host-side history for lanes whose tools cannot execute Git.
use crate::{Result, RunError, artifacts::seal};
use provenant_supervisor::{
    exec::{CapturedStream, ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::Directory,
};
use std::{ffi::OsString, path::Path, time::Duration};

const CONTEXT_LIMIT: usize = 1024 * 1024;

fn execution_error(error: impl std::fmt::Display) -> RunError {
    RunError::Io(std::io::Error::other(error.to_string()))
}

fn read_git(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
    args: &[&str],
    limit: usize,
) -> Result<CapturedStream> {
    let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
        .chain(
            ["--no-pager", "-c", "core.fsmonitor=false"]
                .into_iter()
                .chain(args.iter().copied())
                .map(OsString::from),
        )
        .collect();
    // Replace the environment: inherited Git overrides and global config must
    // not redirect this read or enable a credential/helper process.
    let env: Vec<(OsString, OsString)> = [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("LC_ALL", "C"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(10));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit };
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    options.stop_on_output_limit = true;
    let out = executor.execute(options).map_err(execution_error)?;
    if out.timed_out
        || out.cancelled
        || out.drain_truncated
        || out.stdout.read_failed
        || out.stderr.read_failed
        || out.stderr.truncated
        || (out.termination != Termination::Exit(0)
            && !(out.overflow_stopped && out.stdout.truncated))
    {
        return Err(execution_error("Git context read failed or timed out"));
    }
    Ok(out.stdout)
}

fn resolve(executor: &dyn Executor, git: &Path, cwd: &Path, base: &str) -> Result<String> {
    let invalid = || RunError::InvalidBase(base.to_owned());
    if base.is_empty() || base.len() > 1024 || base.contains('\0') {
        return Err(invalid());
    }
    let out = read_git(
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
    )
    .map_err(|_| invalid())?;
    let text = std::str::from_utf8(&out.bytes).map_err(|_| invalid())?;
    provenant_core::GitOid::parse(text.trim()).map_err(|_| invalid())?;
    if out.truncated {
        return Err(invalid());
    }
    Ok(text.trim().to_owned())
}

/// Validate the base before admission; persist the resolved commit in the
/// immutable task so a moving ref cannot change a queued lane's boundary.
pub fn resolve_git_base(git: &Path, cwd: &Path, base: &str) -> Result<String> {
    let executor = SystemExecutor::new(1).map_err(execution_error)?;
    resolve(&executor, git, cwd, base)
}

pub struct GitContext {
    base: String,
    head: String,
    log: CapturedStream,
    diff: CapturedStream,
}
impl GitContext {
    /// Capture on the host once, before preparing any provider candidate.
    pub fn capture(git: &Path, cwd: &Path, base: &str) -> Result<Self> {
        let executor = SystemExecutor::new(1).map_err(execution_error)?;
        Self::capture_with(&executor, git, cwd, base)
    }

    fn capture_with(executor: &dyn Executor, git: &Path, cwd: &Path, base: &str) -> Result<Self> {
        let base = resolve(executor, git, cwd, base)?;
        let head = resolve(executor, git, cwd, "HEAD")?;
        let range = format!("{base}..{head}");
        let log = read_git(
            executor,
            git,
            cwd,
            &[
                "log",
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
        )?;
        let diff = read_git(
            executor,
            git,
            cwd,
            &[
                "diff",
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
        )?;
        Ok(Self {
            base,
            head,
            log,
            diff,
        })
    }

    /// Files share the candidate's existing scratch grant and terminal cleanup.
    pub fn write_to(&self, scratch: &Path) -> Result<String> {
        let directory = Directory::open_anchor(scratch)?;
        seal(
            &directory,
            "git-log.txt",
            &self.log.bytes,
            CONTEXT_LIMIT as u64,
        )?;
        seal(
            &directory,
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
        }))
        .map_err(execution_error)?;
        seal(&directory, "git-context.json", &metadata, 4096)?;
        Ok(format!(
            "Host-written Git context for {}..{}: log at {}; diff at {}; \
             bounds and truncation at {}. Read these files for the review boundary; \
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
    use provenant_supervisor::exec::{ExecError, ExecOutcome};
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
            assert!(options.env.is_some());
            assert!(options.timeout <= Duration::from_secs(10));
            let mut calls = self.0.lock().map_err(|_| ExecError::Busy)?;
            calls.push(options.argv.to_vec());
            let bytes = if options.argv[4] == "rev-parse" {
                format!("{OID}\n").into_bytes()
            } else {
                assert!(options.argv.contains(&"--no-ext-diff".into()));
                assert!(options.argv.contains(&"--no-textconv".into()));
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
        let fixture = InspectGit(Mutex::new(Vec::new()));
        let argv = [
            "/fixture/git".into(),
            "--no-pager".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "rev-parse".into(),
        ];
        let mut options = ExecOptions::new(&argv, Path::new("/fixture"), Duration::from_secs(1));
        options.stop_on_output_limit = true;
        options.env = Some(&[]);
        let mut out = fixture.execute(options)?;
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
        let prompt = context.write_to(scratch.path())?;
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
        assert_eq!(executor.0.lock().map_err(|_| "calls poisoned")?.len(), 4);
        Ok(())
    }

    fn git(executor: &SystemExecutor, binary: &Path, cwd: &Path, args: &[&str]) -> TestResult {
        read_git(executor, binary, cwd, args, 4096)?;
        Ok(())
    }

    #[test]
    fn git_context_bounds_real_history_and_refuses_bad_base_without_helpers() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir()?;
        let root = std::fs::canonicalize(scratch.path())?;
        let binary = provenant_supervisor::git::git_executable().ok_or("Git unavailable")?;
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
        context.write_to(&output)?;
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
