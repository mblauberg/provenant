//! Schema and execution for the generated-artefact CI handoff.
//!
//! `provenant ci patch` reads the `generated-<head>` artefact that the
//! `generated` CI job uploads, checks it against the run it came from and the
//! worktree it would change, then optionally applies it. Verification is a
//! pure, testable function; Git and forge access live behind a small runtime
//! seam so the head-mismatch and dirty-work contracts can be pinned without a
//! forge account.
use crate::pr::status::batch;
use crate::pr::{forge_execute, successful};
use provenant_cli::Globals;
use provenant_core::{AttemptId, GitOid};
use provenant_records::Layer;
use provenant_records::command_result::{
    CommandError, ErrorClass, NextStep, SettingRef, Subject as ErrorSubject,
};
use provenant_records::custody::{Subject as CustodySubject, SubjectKind, guard_destructive};
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The setting that governs generated patches. A `Configured` refusal names it
/// so the error catalogue's "every configured error has a setting" rule holds.
pub(crate) const GENERATED_PATCH_KEY: &str = "domains.ci.generated_patch";
const LEAF: &str = "ci patch";
const COMMIT_MESSAGE: &str = "chore: apply generated patch";
const READ_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    dead_code,
    reason = "every status is a wire value; only drift is produced locally"
)]
pub(crate) enum GeneratorStatus {
    Clean,
    Drift,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code, reason = "the manifest is a fixed wire schema")]
pub(crate) struct GeneratorManifest {
    pub name: String,
    pub status: GeneratorStatus,
    pub files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code, reason = "the manifest is a fixed wire schema")]
pub(crate) struct GeneratedManifest {
    pub repo: String,
    pub workflow: String,
    pub run_id: u64,
    pub run_attempt: u32,
    pub head_sha: GitOid,
    pub generators: Vec<GeneratorManifest>,
    pub patch_sha256: String,
}

/// The run facts the artefact must agree with before it is trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunIdentity {
    pub repo: String,
    pub workflow: String,
    pub run_attempt: u32,
}

/// What applying the verified patch did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PatchReport {
    pub head: GitOid,
    pub applied: bool,
    pub committed: bool,
    pub paths: Vec<String>,
}

/// The one typed failure shape for this command. `code` is stable for the
/// catalogue; `next` is never empty so the error contract holds.
#[derive(Debug)]
pub(crate) struct PatchError {
    pub code: String,
    pub message: String,
    pub cause: Option<String>,
    pub class: ErrorClass,
    pub setting: Option<SettingRef>,
    pub subject: Option<ErrorSubject>,
    pub next: Vec<NextStep>,
}

impl PatchError {
    fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        cause: Option<String>,
        class: ErrorClass,
        next: Vec<NextStep>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            cause,
            class,
            setting: None,
            subject: None,
            next,
        }
    }

    fn operation(
        code: impl Into<String>,
        message: impl Into<String>,
        cause: Option<String>,
    ) -> Self {
        Self::new(
            code,
            message,
            cause,
            ErrorClass::Operation,
            vec![NextStep::new(
                Vec::new(),
                "correct the reported condition and rerun the command",
            )],
        )
    }

    fn configured(code: &str, message: String, subject: ErrorSubject) -> Self {
        Self {
            code: code.to_owned(),
            message,
            cause: None,
            class: ErrorClass::Configured,
            setting: Some(SettingRef {
                key: GENERATED_PATCH_KEY.to_owned(),
                value: "apply".to_owned(),
                layer: Layer::Default,
                origin: "harness-knobs.json".to_owned(),
            }),
            subject: Some(subject),
            next: vec![NextStep::new(
                Vec::new(),
                "check out the manifest head or rerun ci patch without --apply",
            )],
        }
    }

    fn from_command(error: CommandError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            cause: error.cause,
            class: error.class,
            setting: error.setting,
            subject: error.subject,
            next: error.next,
        }
    }

    /// The public transport error, carrying the same typed facts the renderer
    /// uses for every other command.
    pub(crate) fn into_command_error(self) -> CommandError {
        let Self {
            code,
            message,
            cause,
            class,
            setting,
            subject,
            next,
        } = self;
        let mut error = CommandError::new(
            code,
            message,
            cause.as_deref(),
            "correct the reported condition and rerun the command",
        )
        .with_class(class)
        .with_next(next);
        if let Some(setting) = setting {
            error = error.with_setting(setting);
        }
        if let Some(subject) = subject {
            error = error.with_subject(subject);
        }
        error
    }
}

/// Verify the artefact against the run it came from and the worktree it would
/// change. Every check is data-only: nothing is applied or written here.
pub(crate) fn verify(
    manifest: &GeneratedManifest,
    patch: &[u8],
    expected: &RunIdentity,
    worktree_head: GitOid,
    subject: &ErrorSubject,
) -> Result<(), PatchError> {
    if manifest.repo != expected.repo {
        return Err(PatchError::operation(
            "CI_PATCH_REPO_MISMATCH",
            format!(
                "manifest names repo {} but the run belongs to {}",
                manifest.repo, expected.repo
            ),
            None,
        ));
    }
    if manifest.workflow != expected.workflow {
        return Err(PatchError::operation(
            "CI_PATCH_WORKFLOW_MISMATCH",
            format!(
                "manifest names workflow {} but the run used {}",
                manifest.workflow, expected.workflow
            ),
            None,
        ));
    }
    if manifest.run_attempt != expected.run_attempt {
        return Err(PatchError::operation(
            "CI_PATCH_RUN_MISMATCH",
            format!(
                "manifest is from run attempt {} but the run is at attempt {}",
                manifest.run_attempt, expected.run_attempt
            ),
            None,
        ));
    }
    if manifest.head_sha != worktree_head {
        return Err(PatchError::configured(
            "CI_PATCH_HEAD_MISMATCH",
            format!(
                "manifest head {} does not match worktree HEAD {}",
                manifest.head_sha, worktree_head
            ),
            subject.clone(),
        ));
    }
    let hash = Sha256::digest(patch);
    let digest = provenant_core::hex(&hash);
    if digest != manifest.patch_sha256 {
        return Err(PatchError::operation(
            "CI_PATCH_DIGEST_MISMATCH",
            format!(
                "patch digest {digest} does not match manifest digest {}",
                manifest.patch_sha256
            ),
            None,
        ));
    }
    Ok(())
}

/// The paths a unified diff touches. Only the `+++ b/...` header names a result
/// file; `--- a/...` names its input and deletion hunks use `/dev/null`.
pub(crate) fn patch_paths(patch: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(patch);
    let mut paths = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("+++ b/") else {
            continue;
        };
        let path = rest.split('\t').next().unwrap_or(rest);
        if path.is_empty() || path == "/dev/null" || paths.iter().any(|known| known == path) {
            continue;
        }
        paths.push(path.to_owned());
    }
    paths
}

/// The filesystem and Git facts `ci patch` needs. The pure verification and
/// apply-plan tests inject a fake; production uses [`SystemRuntime`].
pub(crate) trait PatchRuntime {
    fn head(&self, tree: &Path) -> Result<String, String>;
    fn apply_check(&self, tree: &Path, patch: &[u8]) -> Result<(), String>;
    fn apply(&self, tree: &Path, patch: &[u8]) -> Result<(), String>;
    fn commit(&self, tree: &Path, paths: &[String], message: &str) -> Result<(), String>;
    /// The attempt holding a live lease on this tree, when one is recorded.
    fn live_holder(&self, state: &Path, common_dir: &Path, tree: &Path) -> Option<AttemptId>;
}

/// The inputs to one apply, grouped so the operation stays a small seam.
pub(crate) struct ApplyRequest<'a> {
    pub manifest: &'a GeneratedManifest,
    pub patch: &'a [u8],
    pub expected: &'a RunIdentity,
    pub worktree: &'a Path,
    pub state: &'a Path,
    pub common_dir: &'a Path,
    pub commit: bool,
    pub subject: ErrorSubject,
}

/// Re-check the head under the runtime, run the destruction guard, then apply
/// with `git apply --check` first so dirty work and unrelated staged files are
/// preserved on any failure.
pub(crate) fn apply_patch(
    runtime: &dyn PatchRuntime,
    mut request: ApplyRequest<'_>,
) -> Result<PatchReport, PatchError> {
    let head_text = runtime.head(request.worktree).map_err(|error| {
        PatchError::operation(
            "CI_PATCH_HEAD_UNREADABLE",
            "cannot read worktree HEAD",
            Some(error),
        )
    })?;
    let head = GitOid::parse(&head_text).map_err(|error| {
        PatchError::operation(
            "CI_PATCH_HEAD_UNREADABLE",
            "worktree HEAD is not a Git object id",
            Some(error.to_string()),
        )
    })?;
    request.subject.head = Some(head);
    verify(
        request.manifest,
        request.patch,
        request.expected,
        head,
        &request.subject,
    )?;

    let holder = runtime.live_holder(request.state, request.common_dir, request.worktree);
    let custody = CustodySubject::new(
        SubjectKind::Worktree,
        request.worktree.display().to_string(),
    );
    guard_destructive("ci patch apply", &custody, holder.is_some(), holder)
        .map_err(PatchError::from_command)?;

    runtime
        .apply_check(request.worktree, request.patch)
        .map_err(|error| {
            PatchError::operation(
                "CI_PATCH_APPLY_CHECK_FAILED",
                "patch does not apply cleanly; no files were changed",
                Some(error),
            )
        })?;
    runtime
        .apply(request.worktree, request.patch)
        .map_err(|error| {
            PatchError::operation("CI_PATCH_APPLY_FAILED", "cannot apply patch", Some(error))
        })?;

    let paths = patch_paths(request.patch);
    if request.commit {
        runtime
            .commit(request.worktree, &paths, COMMIT_MESSAGE)
            .map_err(|error| {
                PatchError::operation(
                    "CI_PATCH_COMMIT_FAILED",
                    "patch applied but the path-limited commit failed",
                    Some(error),
                )
            })?;
    }
    Ok(PatchReport {
        head,
        applied: true,
        committed: request.commit,
        paths,
    })
}

/// Git execution bound to the selected worktree. The patch travels on stdin,
/// so no scratch copy of it is written to the tree being changed.
pub(crate) struct SystemRuntime;

fn git(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<String, String> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let argv: Vec<std::ffi::OsString> = std::iter::once("git")
        .chain(args.iter().copied())
        .map(std::ffi::OsString::from)
        .collect();
    let mut options = ExecOptions::new(&argv, cwd, READ_BUDGET);
    options.stdin = stdin;
    options.process_group = true;
    options.stdout = StreamPolicy::Capture {
        limit: 8 * 1024 * 1024,
    };
    options.stderr = StreamPolicy::Capture { limit: 256 * 1024 };
    let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let outcome = executor
        .execute(options)
        .map_err(|error| error.to_string())?;
    if outcome.termination != Termination::Exit(0) || outcome.timed_out {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            provenant_core::bounded_line(&String::from_utf8_lossy(&outcome.stderr.bytes), 600)
        ));
    }
    Ok(String::from_utf8_lossy(&outcome.stdout.bytes)
        .trim()
        .to_owned())
}

impl PatchRuntime for SystemRuntime {
    fn head(&self, tree: &Path) -> Result<String, String> {
        git(tree, &["rev-parse", "HEAD"], None)
    }

    fn apply_check(&self, tree: &Path, patch: &[u8]) -> Result<(), String> {
        git(tree, &["apply", "--check"], Some(patch)).map(|_| ())
    }

    fn apply(&self, tree: &Path, patch: &[u8]) -> Result<(), String> {
        git(tree, &["apply"], Some(patch)).map(|_| ())
    }

    fn commit(&self, tree: &Path, paths: &[String], message: &str) -> Result<(), String> {
        let mut add = vec!["add", "--"];
        add.extend(paths.iter().map(String::as_str));
        git(tree, &add, None)?;
        let mut commit = vec!["commit", "-m", message, "--"];
        commit.extend(paths.iter().map(String::as_str));
        git(tree, &commit, None).map(|_| ())
    }

    fn live_holder(&self, state: &Path, common_dir: &Path, tree: &Path) -> Option<AttemptId> {
        let location = provenant_config::ProjectLocation::new(common_dir).ok()?;
        let binding = provenant_config::lookup_project_at_state(state, &location)
            .ok()
            .flatten()?;
        provenant_confinement::worktrees::recorded_writer_holder(state, binding.project_id(), tree)
    }
}

/// Find the Actions run that produced a pull request's generated artefact.
fn run_for_pr(
    state: &Path,
    project: &Path,
    repo: &str,
    number: u64,
    request: &provenant_config::ports::Request,
) -> Result<u64, Box<dyn std::error::Error>> {
    use crate::pr::observer::Observer;
    let mut observer = Observer::open(state, project, repo, request)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
    let pull = observer.resource(&format!("pulls/{number}"), Duration::ZERO, request)?;
    let head = pull["head"]["sha"]
        .as_str()
        .ok_or("pull request named no head sha")?;
    let endpoint = format!("repos/{repo}/actions/runs?head_sha={head}&per_page=1");
    let bytes = successful(forge_execute(
        state,
        project,
        &["gh", "api", &endpoint],
        request,
        false,
        None,
    )?)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    value["workflow_runs"][0]["id"]
        .as_u64()
        .ok_or_else(|| "no Actions run produced this pull request head".into())
}

/// Read the artefact and the run facts it must agree with through the one
/// sourced forge port.
fn fetch(
    state: &Path,
    project: &Path,
    target: &provenant_cli::ci::PatchTarget,
    request: &provenant_config::ports::Request,
) -> Result<(GeneratedManifest, Vec<u8>, RunIdentity), Box<dyn std::error::Error>> {
    let repo = batch::repository(state, project)?;
    let run = match target {
        provenant_cli::ci::PatchTarget::Run { id } => *id,
        provenant_cli::ci::PatchTarget::Pr { number } => {
            run_for_pr(state, project, &repo, *number, request)?
        }
    };

    let endpoint = format!("repos/{repo}/actions/runs/{run}");
    let bytes = successful(forge_execute(
        state,
        project,
        &["gh", "api", &endpoint],
        request,
        false,
        None,
    )?)?;
    let meta: Value = serde_json::from_slice(&bytes)?;
    let head = meta["head_sha"]
        .as_str()
        .ok_or("run metadata named no head sha")?;
    let workflow_id = meta["workflow_id"]
        .as_u64()
        .ok_or("run metadata named no workflow id")?;
    let run_attempt = meta["run_attempt"].as_u64().unwrap_or(1) as u32;
    let manifest_repo = meta["repository"]["full_name"]
        .as_str()
        .unwrap_or(&repo)
        .to_owned();

    let workflow_endpoint = format!("repos/{repo}/actions/workflows/{workflow_id}");
    let bytes = successful(forge_execute(
        state,
        project,
        &["gh", "api", &workflow_endpoint],
        request,
        false,
        None,
    )?)?;
    let workflow_meta: Value = serde_json::from_slice(&bytes)?;
    let workflow_path = workflow_meta["path"]
        .as_str()
        .unwrap_or(".github/workflows/ci.yml");
    let workflow = Path::new(workflow_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ci.yml".to_owned());

    let scratch = tempfile::tempdir()?;
    let scratch_path = scratch
        .path()
        .to_str()
        .ok_or("scratch directory is not UTF-8")?;
    let artifact = format!("generated-{head}");
    let id = run.to_string();
    successful(forge_execute(
        state,
        project,
        &[
            "gh",
            "run",
            "download",
            &id,
            "--name",
            &artifact,
            "-R",
            &repo,
            "--dir",
            scratch_path,
        ],
        request,
        false,
        None,
    )?)?;

    let dir = provenant_os::fs::Directory::open_anchor(scratch.path())?;
    let manifest_bytes = dir.read_bounded(OsStr::new("manifest.json"), 1024 * 1024)?;
    let patch = dir.read_bounded(OsStr::new("patch"), 8 * 1024 * 1024)?;
    let manifest: GeneratedManifest = serde_json::from_slice(&manifest_bytes)?;
    Ok((
        manifest,
        patch,
        RunIdentity {
            repo: manifest_repo,
            workflow,
            run_attempt,
        },
    ))
}

fn query_subject(head: GitOid, target: &provenant_cli::ci::PatchTarget) -> ErrorSubject {
    ErrorSubject {
        host: None,
        project: None,
        graph: None,
        node: None,
        attempt: None,
        item: match target {
            provenant_cli::ci::PatchTarget::Run { id } => Some(format!("run:{id}")),
            provenant_cli::ci::PatchTarget::Pr { number } => Some(format!("pr:{number}")),
        },
        pr: match target {
            provenant_cli::ci::PatchTarget::Pr { number } => Some(*number),
            provenant_cli::ci::PatchTarget::Run { .. } => None,
        },
        head: Some(head),
    }
}

fn emit_error(error: PatchError, globals: &Globals) -> u8 {
    let mut result = CommandResult::new(LEAF, Outcome::Error, ());
    result.next = error.next.clone();
    result.error = Some(error.into_command_error());
    crate::emit(&result, globals, "", false)
}

/// `provenant ci patch`: verify the generated artefact, then apply it only
/// under `--apply` and only after the destruction guard, `git apply --check`
/// and the head re-check all pass.
pub(crate) fn run(command: provenant_cli::ci::Patch, globals: &Globals) -> u8 {
    let fetched = (|| -> Result<(GeneratedManifest, Vec<u8>, RunIdentity, PathBuf, PathBuf), Box<dyn std::error::Error>> {
        let project = crate::roots::selected_project(globals.project.as_deref(), true)?;
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Optional,
        )?;
        let state = resolved.roots.state.as_path();
        let request = provenant_config::ports::Request::within(READ_BUDGET);
        let (manifest, patch, identity) = fetch(state, &project, &command.target, &request)?;
        let worktree = command
            .worktree
            .as_deref()
            .unwrap_or(project.as_path())
            .to_path_buf();
        Ok((manifest, patch, identity, worktree, state.to_path_buf()))
    })();
    let (manifest, patch, identity, worktree, state) = match fetched {
        Ok(fetched) => fetched,
        Err(error) => {
            return crate::fail(
                LEAF,
                Outcome::Error,
                "CI_PATCH_FETCH_FAILED",
                &error.to_string(),
                "check the reported forge read error and rerun provenant ci patch",
                globals,
            );
        }
    };

    let common = match git(
        &worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        None,
    ) {
        Ok(common) => PathBuf::from(common),
        Err(error) => {
            return crate::fail(
                LEAF,
                Outcome::Error,
                "CI_PATCH_HEAD_UNREADABLE",
                &error,
                "run from a Git worktree or pass --worktree",
                globals,
            );
        }
    };
    let subject = query_subject(manifest.head_sha, &command.target);

    if !command.apply {
        let head_text = match SystemRuntime.head(&worktree) {
            Ok(head) => head,
            Err(error) => {
                return emit_error(
                    PatchError::operation(
                        "CI_PATCH_HEAD_UNREADABLE",
                        "cannot read worktree HEAD",
                        Some(error),
                    ),
                    globals,
                );
            }
        };
        let head = match GitOid::parse(&head_text) {
            Ok(head) => head,
            Err(error) => {
                return emit_error(
                    PatchError::operation(
                        "CI_PATCH_HEAD_UNREADABLE",
                        "worktree HEAD is not a Git object id",
                        Some(error.to_string()),
                    ),
                    globals,
                );
            }
        };
        if let Err(error) = verify(&manifest, &patch, &identity, head, &subject) {
            return emit_error(error, globals);
        }
        let report = PatchReport {
            head,
            applied: false,
            committed: false,
            paths: patch_paths(&patch),
        };
        let human = format!(
            "ci patch {}: verified {} paths at {head} (not applied)",
            report.paths.len(),
            manifest.head_sha
        );
        return crate::emit(
            &CommandResult::new(LEAF, Outcome::Ok, report),
            globals,
            &human,
            true,
        );
    }

    match apply_patch(
        &SystemRuntime,
        ApplyRequest {
            manifest: &manifest,
            patch: &patch,
            expected: &identity,
            worktree: &worktree,
            state: &state,
            common_dir: &common,
            commit: command.commit,
            subject,
        },
    ) {
        Ok(report) => {
            let human = format!(
                "ci patch: applied {} path(s) at {}{}",
                report.paths.len(),
                report.head,
                if report.committed {
                    " and committed"
                } else {
                    ""
                }
            );
            crate::emit(
                &CommandResult::new(LEAF, Outcome::Ok, report),
                globals,
                &human,
                false,
            )
        }
        Err(error) => emit_error(error, globals),
    }
}

/// Kept as executable JSON so the contract fixture can be consumed by the
/// batch-level tests without coupling them to CI or a forge account.
#[cfg(test)]
pub(crate) const MANIFEST_FIXTURE: &str = r#"{
  "repo": "owner/repo",
  "workflow": "ci.yml",
  "run_id": 123,
  "run_attempt": 1,
  "head_sha": "0123456789abcdef0123456789abcdef01234567",
  "generators": [
    {"name": "swift-fixtures", "status": "drift", "files": ["fixtures/a.swift"]}
  ],
  "patch_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";

    fn fixture(head: &str) -> (GeneratedManifest, Vec<u8>, RunIdentity, ErrorSubject) {
        let patch = b"diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n"
            .to_vec();
        let hash = Sha256::digest(&patch);
        let digest = provenant_core::hex(&hash);
        let manifest: GeneratedManifest = serde_json::from_str(&format!(
            r#"{{
              "repo": "owner/repo",
              "workflow": "ci.yml",
              "run_id": 123,
              "run_attempt": 1,
              "head_sha": "{head}",
              "generators": [],
              "patch_sha256": "{digest}"
            }}"#
        ))
        .unwrap();
        let identity = RunIdentity {
            repo: "owner/repo".to_owned(),
            workflow: "ci.yml".to_owned(),
            run_attempt: 1,
        };
        let subject = ErrorSubject {
            host: None,
            project: None,
            graph: None,
            node: None,
            attempt: None,
            item: Some("run:123".to_owned()),
            pr: None,
            head: None,
        };
        (manifest, patch, identity, subject)
    }

    fn target() -> provenant_cli::ci::PatchTarget {
        provenant_cli::ci::PatchTarget::Run { id: 123 }
    }

    #[test]
    fn manifest_fixture_covers_generated_patch_identity() {
        let manifest: GeneratedManifest = serde_json::from_str(MANIFEST_FIXTURE).unwrap();
        assert_eq!(manifest.repo, "owner/repo");
        assert_eq!(manifest.run_attempt, 1);
        assert_eq!(manifest.generators[0].status, GeneratorStatus::Drift);
        assert_eq!(manifest.generators[0].files, ["fixtures/a.swift"]);
    }

    #[test]
    fn ci_patch_refuses_head_or_run_mismatch() {
        let (manifest, patch, identity, subject) = fixture(HEAD);
        let head = GitOid::parse(HEAD).unwrap();
        assert!(verify(&manifest, &patch, &identity, head, &subject).is_ok());

        // Head mismatch is a Configured error naming both heads, with a next
        // step and a setting so the error catalogue contract holds.
        let other = GitOid::parse(OTHER).unwrap();
        let error = verify(&manifest, &patch, &identity, other, &subject).unwrap_err();
        assert_eq!(error.code, "CI_PATCH_HEAD_MISMATCH");
        assert_eq!(error.class, ErrorClass::Configured);
        assert!(error.message.contains(HEAD));
        assert!(error.message.contains(OTHER));
        assert!(!error.next.is_empty());
        assert_eq!(error.setting.as_ref().unwrap().key, GENERATED_PATCH_KEY);

        // Repo, workflow and run-attempt mismatches carry their own codes.
        let wrong_repo = RunIdentity {
            repo: "other/repo".into(),
            ..identity.clone()
        };
        assert_eq!(
            verify(&manifest, &patch, &wrong_repo, head, &subject)
                .unwrap_err()
                .code,
            "CI_PATCH_REPO_MISMATCH"
        );
        let wrong_workflow = RunIdentity {
            workflow: "other.yml".into(),
            ..identity.clone()
        };
        assert_eq!(
            verify(&manifest, &patch, &wrong_workflow, head, &subject)
                .unwrap_err()
                .code,
            "CI_PATCH_WORKFLOW_MISMATCH"
        );
        let wrong_attempt = RunIdentity {
            run_attempt: 2,
            ..identity
        };
        assert_eq!(
            verify(&manifest, &patch, &wrong_attempt, head, &subject)
                .unwrap_err()
                .code,
            "CI_PATCH_RUN_MISMATCH"
        );
    }

    struct Fake {
        head: String,
        check_error: Option<String>,
        applied: RefCell<Vec<Vec<u8>>>,
        committed: RefCell<Vec<(Vec<String>, String)>>,
        holder: Option<AttemptId>,
    }

    impl PatchRuntime for Fake {
        fn head(&self, _: &Path) -> Result<String, String> {
            Ok(self.head.clone())
        }
        fn apply_check(&self, _: &Path, _: &[u8]) -> Result<(), String> {
            match &self.check_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
        fn apply(&self, _: &Path, patch: &[u8]) -> Result<(), String> {
            self.applied.borrow_mut().push(patch.to_vec());
            Ok(())
        }
        fn commit(&self, _: &Path, paths: &[String], message: &str) -> Result<(), String> {
            self.committed
                .borrow_mut()
                .push((paths.to_vec(), message.to_owned()));
            Ok(())
        }
        fn live_holder(&self, _: &Path, _: &Path, _: &Path) -> Option<AttemptId> {
            self.holder
        }
    }

    fn fake(head: &str, check_error: Option<String>) -> Fake {
        Fake {
            head: head.to_owned(),
            check_error,
            applied: RefCell::new(Vec::new()),
            committed: RefCell::new(Vec::new()),
            holder: None,
        }
    }

    #[test]
    fn ci_patch_preserves_dirty_and_unrelated_staged() {
        let (manifest, patch, identity, subject) = fixture(HEAD);
        let tree = Path::new("/worktree");
        let state = Path::new("/state");
        let common = Path::new("/common");

        // An unclean tree fails `git apply --check`: no apply, no commit, and
        // the error says local work was not touched.
        let dirty = fake(HEAD, Some("local changes would be overwritten".into()));
        let error = apply_patch(
            &dirty,
            ApplyRequest {
                manifest: &manifest,
                patch: &patch,
                expected: &identity,
                worktree: tree,
                state,
                common_dir: common,
                commit: true,
                subject: subject.clone(),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "CI_PATCH_APPLY_CHECK_FAILED");
        assert!(error.message.contains("no files were changed"));
        assert!(dirty.applied.borrow().is_empty());
        assert!(dirty.committed.borrow().is_empty());

        // A clean tree applies, and --commit uses a pathspec limited to the
        // patch's own paths, leaving unrelated staged files alone.
        let clean = fake(HEAD, None);
        let report = apply_patch(
            &clean,
            ApplyRequest {
                manifest: &manifest,
                patch: &patch,
                expected: &identity,
                worktree: tree,
                state,
                common_dir: common,
                commit: true,
                subject,
            },
        )
        .unwrap();
        assert_eq!(report.paths, ["src/a.rs"]);
        assert!(report.committed && report.applied);
        assert_eq!(clean.applied.borrow().len(), 1);
        assert_eq!(clean.committed.borrow()[0].0, vec!["src/a.rs".to_owned()]);

        // A live holder's work refuses before any Git mutation.
        let mut held = fake(HEAD, None);
        held.holder = Some(AttemptId::from_bytes([7; 16]));
        let error = apply_patch(
            &held,
            ApplyRequest {
                manifest: &manifest,
                patch: &patch,
                expected: &identity,
                worktree: tree,
                state,
                common_dir: common,
                commit: false,
                subject: query_subject(GitOid::parse(HEAD).unwrap(), &target()),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "DESTROYS_LIVE_WORK");
        assert!(held.applied.borrow().is_empty());
    }
}
