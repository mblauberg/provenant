//! Real forge, git, gate and alarm adapters for the landing driver.
use super::{
    LandingError,
    model::*,
    ports::{
        AlarmSink, Approval, Forge, GateRunner, MergeTree, PullRequestInfo, PullRequestState, Repo,
    },
};
use crate::pr::{TimedOut, execute_with, git::Git, git::plain_ref, merge_with, rest_pull, wait};
use provenant_cli::domains::MergeMethod;
use provenant_os::exec::Termination;
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

/// The side-effect adapters used by the landing driver.
pub type Adapters = (
    Box<dyn Forge>,
    Box<dyn Repo>,
    Box<dyn GateRunner>,
    Box<dyn AlarmSink>,
);

/// Construct the real forge, repository, gate and alarm adapters for a project.
pub fn connect(project: &Path) -> Result<Adapters, LandingError> {
    Ok((
        Box::new(GhForge::new(project)),
        Box::new(GitRepo::new(project)?),
        Box::new(CheckGate::new(project)),
        Box::new(ChairAlarms::new(project)),
    ))
}

fn failed(error: Box<dyn std::error::Error>) -> LandingError {
    LandingError::Failed(error.to_string())
}
/// A lost or slow mutation leaves the outcome unknown rather than failed.
fn mutation_failed(error: Box<dyn std::error::Error>) -> LandingError {
    if error.is::<TimedOut>() {
        LandingError::PublicationUnknown
    } else if error
        .downcast_ref::<wait::Failure>()
        .is_some_and(|failure| matches!(failure.code, "HEAD_MOVED" | "MERGE_NOT_ALLOWED"))
        || error.to_string().contains("HTTP 422")
    {
        LandingError::Refused(error.to_string())
    } else {
        failed(error)
    }
}
fn object_id(value: &str) -> Result<&str, LandingError> {
    let valid = matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit());
    if valid {
        Ok(value)
    } else {
        Err(LandingError::Failed(format!(
            "not a full object id: {value}"
        )))
    }
}

// ----- Forge -------------------------------------------------------------

/// GitHub through the `gh` REST edge, with the shared pacing and rate-limit retry.
pub struct GhForge {
    project: PathBuf,
    budget: Duration,
}
impl GhForge {
    pub fn new(project: &Path) -> Self {
        Self {
            project: project.to_owned(),
            budget: Duration::from_secs(120),
        }
    }
    #[allow(clippy::disallowed_methods)]
    fn reader(&self) -> wait::Reader<'_> {
        wait::Reader {
            project: &self.project,
            deadline: std::time::Instant::now() + self.budget,
            retry_command: "provenant land".into(),
        }
    }
    fn get(&self, args: &[&str]) -> Result<Vec<u8>, LandingError> {
        let mut argv = vec!["gh", "api"];
        argv.extend_from_slice(args);
        let output = self.reader().read(&argv).map_err(failed)?;
        if output.termination != Termination::Exit(0) {
            return Err(LandingError::Failed(format!(
                "GitHub read failed: {}",
                String::from_utf8_lossy(&output.stderr.bytes).trim()
            )));
        }
        Ok(output.stdout.bytes)
    }
    fn json(&self, endpoint: &str) -> Result<serde_json::Value, LandingError> {
        serde_json::from_slice(&self.get(&[endpoint])?)
            .map_err(|error| LandingError::Failed(format!("GitHub returned invalid JSON: {error}")))
    }
    /// Every page of a list endpoint, flattened.
    fn pages(&self, endpoint: &str) -> Result<Vec<serde_json::Value>, LandingError> {
        let pages: Vec<serde_json::Value> =
            serde_json::from_slice(&self.get(&["--paginate", "--slurp", endpoint])?).map_err(
                |error| LandingError::Failed(format!("GitHub returned invalid JSON: {error}")),
            )?;
        Ok(pages
            .into_iter()
            .flat_map(|page| match page {
                serde_json::Value::Array(items) => items,
                other => vec![other],
            })
            .collect())
    }
}

/// Fold per-check results: any failure fails, then pending, then missing,
/// then a skipped or neutral check; only an all-passed set passes.
pub(super) fn fold_results(results: impl IntoIterator<Item = GateResult>) -> GateResult {
    let results: Vec<GateResult> = results.into_iter().collect();
    for rank in [
        GateResult::Failed,
        GateResult::Pending,
        GateResult::Missing,
        GateResult::Skipped,
        GateResult::Neutral,
    ] {
        if results.contains(&rank) {
            return rank;
        }
    }
    if results.is_empty() {
        GateResult::Missing
    } else {
        GateResult::Passed
    }
}
fn merge_label(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "merge",
        MergeMethod::Squash => "squash",
        MergeMethod::Rebase => "rebase",
    }
}
/// Rule types that do not stop a fast-forward push of a verified candidate.
const HARMLESS_RULES: [&str; 4] = [
    "deletion",
    "non_fast_forward",
    "creation",
    "required_linear_history",
];
/// Whether direct push is allowed, or the reason it is not.
fn direct_push_verdict(
    repository: &serde_json::Value,
    branch: &serde_json::Value,
    rules: &[serde_json::Value],
) -> Result<(), String> {
    if repository["permissions"]["push"].as_bool() != Some(true) {
        return Err("the landing identity has no push permission".into());
    }
    match branch["protected"].as_bool() {
        Some(false) => {}
        Some(true) => {
            return Err("the target has branch protection; push authority is unknown".into());
        }
        None => return Err("branch protection state is unknown".into()),
    }
    for rule in rules {
        let kind = rule["type"].as_str().unwrap_or("unknown");
        if !HARMLESS_RULES.contains(&kind) {
            return Err(format!("a `{kind}` ruleset rule applies to the target"));
        }
    }
    Ok(())
}
fn encode_branch(branch: &str) -> String {
    branch
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

impl Forge for GhForge {
    fn pull_request(&mut self, number: u64) -> Result<PullRequestInfo, LandingError> {
        let pull =
            rest_pull(&|argv| self.reader().read(argv), &number.to_string()).map_err(failed)?;
        let merged = pull.merged_at.is_some();
        Ok(PullRequestInfo {
            number,
            head: pull.head.sha.to_string(),
            base_branch: pull.base.branch,
            author: pull.user.map(|user| user.login).unwrap_or_default(),
            state: if merged {
                PullRequestState::Merged
            } else if pull.state == "open" {
                PullRequestState::Open
            } else {
                PullRequestState::Closed
            },
            merged_commit: if merged { pull.merge_commit_sha } else { None },
        })
    }
    fn target_branch(&mut self) -> Result<String, LandingError> {
        self.json("repos/{owner}/{repo}")?["default_branch"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| LandingError::Failed("repository has no default branch".into()))
    }
    fn head(&mut self, number: u64) -> Result<String, LandingError> {
        Ok(self.pull_request(number)?.head)
    }
    fn approvals(&mut self, number: u64, head: &str) -> Result<Vec<Approval>, LandingError> {
        let reviews = self.pages(&format!(
            "repos/{{owner}}/{{repo}}/pulls/{number}/reviews?per_page=100"
        ))?;
        // The latest decisive review of each reviewer stands; comments and
        // pending reviews neither grant nor withdraw approval.
        let mut latest: std::collections::BTreeMap<String, Option<Approval>> = Default::default();
        for review in &reviews {
            let Some(reviewer) = review["user"]["login"].as_str() else {
                continue;
            };
            let approved = match review["state"].as_str() {
                Some("APPROVED") => true,
                Some("CHANGES_REQUESTED" | "DISMISSED") => false,
                _ => continue,
            };
            let at_head = review["commit_id"].as_str() == Some(head);
            latest.insert(
                reviewer.to_owned(),
                (approved && at_head).then(|| Approval {
                    reference: review["id"].to_string(),
                    reviewer: reviewer.to_owned(),
                }),
            );
        }
        Ok(latest.into_values().flatten().collect())
    }
    fn checks(&mut self, commit: &str) -> Result<Vec<GateEvidence>, LandingError> {
        let commit = object_id(commit)?;
        let head = commit
            .parse()
            .map_err(|_| LandingError::Failed("invalid commit id".into()))?;
        let runs = wait::commit_checks("repos/{owner}/{repo}", head, &|args| {
            self.reader().read(args)
        })
        .map_err(failed)?;
        let tree =
            self.json(&format!("repos/{{owner}}/{{repo}}/git/commits/{commit}"))?["tree"]["sha"]
                .as_str()
                .ok_or_else(|| LandingError::Failed("commit has no tree".into()))?
                .to_owned();
        let checks = runs
            .into_iter()
            .map(|run| CheckEvidence {
                name: run.name,
                issuer: run.app_slug,
                result: match run.bucket.as_str() {
                    "pass" => GateResult::Passed,
                    "pending" => GateResult::Pending,
                    "skipping" if run.conclusion.as_deref() == Some("neutral") => {
                        GateResult::Neutral
                    }
                    "skipping" => GateResult::Skipped,
                    _ => GateResult::Failed,
                },
            })
            .collect::<Vec<_>>();
        let result = fold_results(checks.iter().map(|check| check.result.clone()));
        Ok(vec![GateEvidence {
            commit: commit.to_owned(),
            tree,
            definition_digest: String::new(),
            checks,
            issuer: None,
            coverage: Vec::new(),
            result,
            pool: None,
            duration_seconds: None,
        }])
    }
    fn merge(
        &mut self,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<String, LandingError> {
        let pinned = object_id(head)?
            .parse()
            .map_err(|_| LandingError::Failed(format!("not a commit id: {head}")))?;
        let sha = merge_with(&self.reader(), &number.to_string(), pinned, method)
            .map_err(mutation_failed)?;
        sha.ok_or_else(|| {
            LandingError::Failed(format!(
                "GitHub merged #{number} by {} without naming the commit",
                merge_label(method)
            ))
        })
    }
    fn may_push_directly(&mut self, branch: &str) -> Result<bool, LandingError> {
        let verdict = (|| -> Result<(), String> {
            let encoded = encode_branch(branch);
            let read = |endpoint: &str| self.json(endpoint).map_err(|e| e.to_string());
            let repository = read("repos/{owner}/{repo}")?;
            let info = read(&format!("repos/{{owner}}/{{repo}}/branches/{encoded}"))?;
            let rules = self
                .pages(&format!(
                    "repos/{{owner}}/{{repo}}/rules/branches/{encoded}?per_page=100"
                ))
                .map_err(|e| e.to_string())?;
            direct_push_verdict(&repository, &info, &rules)
        })();
        if let Err(reason) = &verdict {
            // Unknown is not allowed; say why so the fallback is not a mystery.
            let _ = writeln!(
                std::io::stderr().lock(),
                "direct push to {branch} not allowed: {reason}"
            );
        }
        Ok(verdict.is_ok())
    }
    fn supports_revert_forward(&self) -> bool {
        true
    }
    fn open_revert(
        &mut self,
        number: u64,
        branch: &str,
        target: &str,
        checks: &str,
    ) -> Result<u64, LandingError> {
        let title = format!("Revert pull request #{number}");
        let body =
            format!("Reverts pull request #{number} after post-merge check failure: {checks}.");
        let bytes = self.get(&[
            "repos/{owner}/{repo}/pulls",
            "--method",
            "POST",
            "-f",
            &format!("title={title}"),
            "-f",
            &format!("body={body}"),
            "-f",
            &format!("head={branch}"),
            "-f",
            &format!("base={target}"),
        ])?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
            LandingError::Failed(format!("invalid revert pull request response: {e}"))
        })?;
        value["number"].as_u64().ok_or_else(|| {
            LandingError::Failed("revert pull request response has no number".into())
        })
    }
    fn labels(&mut self, number: u64) -> Result<Vec<String>, LandingError> {
        Ok(self
            .pages(&format!(
                "repos/{{owner}}/{{repo}}/issues/{number}/labels?per_page=100"
            ))?
            .iter()
            .filter_map(|label| label["name"].as_str().map(str::to_owned))
            .collect())
    }
}

// ----- Repo --------------------------------------------------------------

/// Git plumbing in the project checkout. Refs live on the remote the target tracks.
pub struct GitRepo {
    project: PathBuf,
    git: Git,
    remote: Option<String>,
}
impl GitRepo {
    pub fn new(project: &Path) -> Result<Self, LandingError> {
        Ok(Self {
            project: project.to_owned(),
            git: Git::new().map_err(failed)?,
            remote: None,
        })
    }
    fn remote(&mut self, branch: &str) -> Result<String, LandingError> {
        if let Some(remote) = &self.remote {
            return Ok(remote.clone());
        }
        let remote = self.git.remote_of(&self.project, branch).map_err(failed)?;
        self.remote = Some(remote.clone());
        Ok(remote)
    }
    fn exit(&self, args: &[&str]) -> Result<(i64, String, String), LandingError> {
        self.git.exit(&self.project, args).map_err(failed)
    }
    fn ok(&self, args: &[&str]) -> Result<String, LandingError> {
        let (code, out, error) = self.exit(args)?;
        if code == 0 {
            Ok(out.trim().to_owned())
        } else {
            Err(LandingError::Failed(format!(
                "git {} failed: {error}",
                args.first().copied().unwrap_or_default()
            )))
        }
    }
    /// Fetch a commit by id when it is not already present.
    fn ensure(&mut self, commit: &str) -> Result<(), LandingError> {
        let commit = object_id(commit)?;
        let probe = format!("{commit}^{{commit}}");
        if self.exit(&["cat-file", "-e", &probe])?.0 == 0 {
            return Ok(());
        }
        let remote = self.remote("")?;
        self.ok(&["fetch", "--no-tags", "--quiet", &remote, commit])
            .map(|_| ())
    }
    fn branch_ref(name: &str) -> Result<String, LandingError> {
        if !plain_ref(name) || name.starts_with("refs/") {
            return Err(LandingError::Failed(format!("unsafe ref name: {name}")));
        }
        Ok(format!("refs/heads/{name}"))
    }
    fn lease_push(
        &mut self,
        full: &str,
        refspec: &str,
        expected: Option<&str>,
    ) -> Result<(), LandingError> {
        let lease = format!(
            "--force-with-lease={full}:{}",
            expected.map(object_id).transpose()?.unwrap_or_default()
        );
        let remote = self.remote("")?;
        if expected.is_none() {
            let existing = self.ok(&["ls-remote", "--refs", &remote, full])?;
            if !existing.is_empty() {
                return Err(LandingError::Refused(format!(
                    "remote ref already exists: {full}"
                )));
            }
        }
        match self.git.exit(
            &self.project,
            &["push", "--porcelain", &lease, &remote, refspec],
        ) {
            Ok((0, ..)) => Ok(()),
            Ok((_, output, error))
                if output.contains("[remote rejected]")
                    || output.contains("[rejected]")
                        && (output.contains("stale info")
                            || output.contains("non-fast-forward")
                            || output.contains("fetch first"))
                    || error.contains("[remote rejected]")
                    || error.contains("[rejected]")
                        && (error.contains("stale info")
                            || error.contains("non-fast-forward")
                            || error.contains("fetch first")) =>
            {
                Err(LandingError::Refused(format!("git push refused: {error}")))
            }
            Ok((_, _, error)) => Err(LandingError::Failed(format!("git push failed: {error}"))),
            Err(error) => Err(mutation_failed(error)),
        }
    }
}

impl Repo for GitRepo {
    fn repository_identity(&mut self, target: &str) -> Result<String, LandingError> {
        use sha2::{Digest, Sha256};
        let remote = self.remote(target)?;
        let url = self.ok(&["remote", "get-url", &remote])?;
        Ok(Sha256::digest(url.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
    fn fetch_target(&mut self, branch: &str) -> Result<String, LandingError> {
        let full = Self::branch_ref(branch)?;
        let remote = self.remote(branch)?;
        self.ok(&["fetch", "--no-tags", "--quiet", "--refmap=", &remote, &full])?;
        let peeled = "FETCH_HEAD^{commit}".to_owned();
        self.ok(&["rev-parse", "--verify", &peeled])
    }
    fn fetch_target_untracked(&mut self, branch: &str) -> Result<String, LandingError> {
        let full = Self::branch_ref(branch)?;
        let remote = self.remote(branch)?;
        self.ok(&["fetch", "--no-tags", "--quiet", "--refmap=", &remote, &full])?;
        self.ok(&["rev-parse", "--verify", "FETCH_HEAD^{commit}"])
    }
    fn revert_commit(&mut self, target: &str, landed: &str) -> Result<String, LandingError> {
        self.ensure(target)?;
        self.ensure(landed)?;
        let parents = self.ok(&["rev-list", "--parents", "-n", "1", object_id(landed)?])?;
        if parents.split_whitespace().count() != 3 || !self.is_ancestor(landed, target)? {
            return Err(LandingError::Failed(
                "culprit must be a two-parent merge commit on the target".into(),
            ));
        }
        static SCRATCH_SEQUENCE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let scratch = loop {
            let nonce = SCRATCH_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("provenant-revert-{}-{nonce}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(LandingError::Failed(format!(
                        "create revert scratch: {error}"
                    )));
                }
            }
        };
        let path = scratch
            .to_str()
            .ok_or_else(|| LandingError::Failed("scratch path is not UTF-8".into()))?;
        let added = self.ok(&[
            "worktree",
            "add",
            "--detach",
            "--quiet",
            path,
            object_id(target)?,
        ]);
        if let Err(error) = added {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(error);
        }
        let result = (|| {
            let (code, _, error) = self
                .git
                .exit(&scratch, &["revert", "--no-commit", "-m", "1", landed])
                .map_err(failed)?;
            if code != 0 {
                return Err(LandingError::Failed(format!("revert failed: {error}")));
            }
            let (code, tree, error) = self.git.exit(&scratch, &["write-tree"]).map_err(failed)?;
            if code != 0 {
                return Err(LandingError::Failed(format!("write revert tree: {error}")));
            }
            self.commit_tree(
                tree.trim(),
                &[target.to_owned()],
                &format!("Revert merge {landed}"),
            )
        })();
        let cleanup = self.ok(&["worktree", "remove", "--force", path]);
        match (result, cleanup) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(commit), Ok(_)) => Ok(commit),
        }
    }
    fn merge_tree(&mut self, base: &str, head: &str) -> Result<MergeTree, LandingError> {
        self.ensure(base)?;
        self.ensure(head)?;
        let (code, out, error) = self.exit(&[
            "merge-tree",
            "--write-tree",
            "--no-messages",
            object_id(base)?,
            object_id(head)?,
        ])?;
        match code {
            0 => out
                .lines()
                .next()
                .map(|tree| MergeTree::Tree(tree.trim().to_owned()))
                .ok_or_else(|| LandingError::Failed("merge-tree printed no tree".into())),
            1 => Ok(MergeTree::Conflict),
            _ => Err(LandingError::Failed(format!(
                "git merge-tree failed: {error}"
            ))),
        }
    }
    fn commit_tree(
        &mut self,
        tree: &str,
        parents: &[String],
        message: &str,
    ) -> Result<String, LandingError> {
        let mut args = vec!["commit-tree", object_id(tree)?];
        for parent in parents {
            args.push("-p");
            args.push(object_id(parent)?);
        }
        args.extend(["-m", message]);
        self.ok(&args)
    }
    fn push_ref(
        &mut self,
        name: &str,
        commit: &str,
        expected_old: Option<&str>,
    ) -> Result<(), LandingError> {
        let full = Self::branch_ref(name)?;
        let refspec = format!("{}:{full}", object_id(commit)?);
        self.lease_push(&full, &refspec, expected_old)
    }
    fn delete_ref(&mut self, name: &str, expected_old: &str) -> Result<(), LandingError> {
        let full = Self::branch_ref(name)?;
        let refspec = format!(":{full}");
        match self.lease_push(&full, &refspec, Some(expected_old)) {
            Ok(()) => Ok(()),
            Err(error) => {
                // A previous cleanup may have succeeded despite a lost response.
                // Only absence is idempotent; a ref at a different value stays untouched.
                let remote = self.remote("")?;
                match self.exit(&["ls-remote", "--exit-code", "--heads", &remote, &full]) {
                    Ok((2, _, _)) => Ok(()),
                    _ => Err(error),
                }
            }
        }
    }
    fn is_ancestor(&mut self, ancestor: &str, descendant: &str) -> Result<bool, LandingError> {
        self.ensure(ancestor)?;
        self.ensure(descendant)?;
        let (code, _, error) = self.exit(&[
            "merge-base",
            "--is-ancestor",
            object_id(ancestor)?,
            object_id(descendant)?,
        ])?;
        match code {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(LandingError::Failed(format!(
                "git merge-base failed: {error}"
            ))),
        }
    }
    fn tree_of(&mut self, commit: &str) -> Result<String, LandingError> {
        self.ensure(commit)?;
        let spec = format!("{}^{{tree}}", object_id(commit)?);
        self.ok(&["rev-parse", "--verify", &spec])
    }
}

// ----- GateRunner --------------------------------------------------------

/// Observes forge check runs on the exact candidate, or runs the command plan.
pub struct CheckGate {
    project: PathBuf,
    forge: GhForge,
}
impl CheckGate {
    pub fn new(project: &Path) -> Self {
        Self {
            project: project.to_owned(),
            forge: GhForge::new(project),
        }
    }
}
/// Evidence for the required checks only, bound to the definition.
fn bind_evidence(
    candidate: &Candidate,
    definition: &GateDefinition,
    observed: &GateEvidence,
) -> Result<GateEvidence, LandingError> {
    if observed.commit != candidate.commit || observed.tree != candidate.tree {
        return Err(LandingError::Failed(
            "observed checks do not belong to the candidate commit and tree".into(),
        ));
    }
    let checks: Vec<CheckEvidence> = definition
        .checks
        .iter()
        .map(|name| {
            let matching = observed.checks.iter().filter(|check| {
                check.name == *name
                    && definition
                        .issuer
                        .as_ref()
                        .is_none_or(|issuer| check.issuer.as_ref() == Some(issuer))
            });
            let results: Vec<GateResult> = matching.clone().map(|c| c.result.clone()).collect();
            CheckEvidence {
                name: name.clone(),
                issuer: matching.filter_map(|check| check.issuer.clone()).next(),
                result: if results.is_empty() {
                    GateResult::Missing
                } else {
                    fold_results(results)
                },
            }
        })
        .collect();
    let matching: Vec<_> = observed
        .checks
        .iter()
        .filter(|check| {
            definition.checks.contains(&check.name)
                && definition
                    .issuer
                    .as_ref()
                    .is_none_or(|issuer| check.issuer.as_ref() == Some(issuer))
        })
        .collect();
    let issuers: std::collections::BTreeSet<_> = matching
        .iter()
        .filter_map(|check| check.issuer.clone())
        .collect();
    let observed_issuer = if !matching.is_empty()
        && matching.iter().all(|check| check.issuer.is_some())
        && issuers.len() == 1
    {
        issuers.into_iter().next()
    } else {
        None
    };
    Ok(GateEvidence {
        commit: observed.commit.clone(),
        tree: observed.tree.clone(),
        definition_digest: definition.digest.clone(),
        result: fold_results(checks.iter().map(|check| check.result.clone())),
        issuer: definition
            .issuer
            .clone()
            .or_else(|| observed_issuer.clone()),
        pool: definition.pool.clone().or(observed_issuer),
        checks,
        coverage: definition.coverage.clone(),
        duration_seconds: None,
    })
}
/// Poll until the required checks settle or the budget (summed pauses) ends.
pub(super) fn observe_checks(
    forge: &mut dyn Forge,
    candidate: &Candidate,
    definition: &GateDefinition,
    poll_seconds: u64,
    sleep: &mut dyn FnMut(Duration),
) -> Result<GateEvidence, LandingError> {
    observe_checks_with(candidate, definition, poll_seconds, sleep, &mut || {
        forge.checks(&candidate.commit)
    })
}

fn observe_checks_with(
    candidate: &Candidate,
    definition: &GateDefinition,
    poll_seconds: u64,
    sleep: &mut dyn FnMut(Duration),
    read: &mut dyn FnMut() -> Result<Vec<GateEvidence>, LandingError>,
) -> Result<GateEvidence, LandingError> {
    let poll = poll_seconds.max(1);
    let mut elapsed = 0;
    loop {
        let observed = read()?
            .into_iter()
            .next()
            .ok_or_else(|| LandingError::Failed("forge returned no check evidence".into()))?;
        let mut evidence = bind_evidence(candidate, definition, &observed)?;
        evidence.duration_seconds = Some(elapsed);
        let settled = !matches!(evidence.result, GateResult::Pending | GateResult::Missing);
        if settled || elapsed >= definition.timeout_seconds {
            return Ok(evidence);
        }
        sleep(Duration::from_secs(poll));
        elapsed += poll;
    }
}
impl GateRunner for CheckGate {
    #[allow(clippy::disallowed_methods)]
    fn gate(
        &mut self,
        candidate: &Candidate,
        definition: &GateDefinition,
        poll_seconds: u64,
    ) -> Result<GateEvidence, LandingError> {
        match definition.kind {
            provenant_cli::domains::GateKind::Checks => observe_checks(
                &mut self.forge,
                candidate,
                definition,
                poll_seconds,
                &mut std::thread::sleep,
            ),
            provenant_cli::domains::GateKind::Commands => {
                super::gate::execute_commands(&self.project, candidate, definition)
            }
        }
    }
}

// ----- AlarmSink ---------------------------------------------------------

/// Posts through the chair inbox (`provenant chair message`) when a chair
/// session is known; an alarm is always also written to standard error.
pub struct ChairAlarms {
    project: PathBuf,
    session: Option<String>,
}
impl ChairAlarms {
    pub fn new(project: &Path) -> Self {
        Self {
            project: project.to_owned(),
            session: std::env::var("PROVENANT_SESSION_ID")
                .ok()
                .filter(|value| !value.is_empty()),
        }
    }
}
fn alarm_text(alarm: &Alarm) -> String {
    format!("landing alarm {}: {}", alarm.code, alarm.message)
        .chars()
        .take(4000)
        .collect()
}
impl AlarmSink for ChairAlarms {
    fn post(&mut self, alarm: &Alarm) -> Result<(), LandingError> {
        let text = alarm_text(alarm);
        let _ = writeln!(std::io::stderr().lock(), "{text}");
        deliver_alarm(self.session.as_deref(), &text, &mut |session, text| {
            let executable = std::env::current_exe().map_err(|error| {
                LandingError::Failed(format!("cannot locate provenant: {error}"))
            })?;
            let executable = executable.to_string_lossy();
            let output = execute_with(
                &self.project,
                &[
                    &executable,
                    "chair",
                    "message",
                    session,
                    "--text-stdin",
                    "--channel",
                    "cli",
                ],
                Duration::from_secs(30),
                false,
                Some(text.as_bytes()),
            )
            .map_err(failed)?;
            if output.termination != Termination::Exit(0) {
                return Err(LandingError::Failed(format!(
                    "chair message failed: {}",
                    String::from_utf8_lossy(&output.stderr.bytes).trim()
                )));
            }
            Ok(())
        })
    }
}

fn deliver_alarm(
    session: Option<&str>,
    text: &str,
    send: &mut dyn FnMut(&str, &str) -> Result<(), LandingError>,
) -> Result<(), LandingError> {
    if let Some(session) = session {
        send(session, text)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;
    use crate::pr::landing::fakes::FakeForge;
    use serde_json::json;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
    fn commit_file(dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", file]);
        git(dir, &["rev-parse", "HEAD"])
    }
    struct Rig {
        _dir: tempfile::TempDir,
        work: PathBuf,
        bare: PathBuf,
        repo: GitRepo,
    }
    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("remote.git");
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        git(
            dir.path(),
            &["init", "-q", "--bare", "-b", "main", "remote.git"],
        );
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["config", "commit.gpgsign", "false"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        commit_file(&work, "a", "a\n");
        git(&work, &["push", "-q", "origin", "main"]);
        let repo = GitRepo::new(&work).unwrap();
        Rig {
            _dir: dir,
            work,
            bare,
            repo,
        }
    }

    #[test]
    fn revert_commit_uses_current_target_and_publishes_only_new_branch() {
        let mut rig = rig();
        git(&rig.work, &["config", "user.name", "test"]);
        git(&rig.work, &["config", "user.email", "test@example.invalid"]);
        git(&rig.work, &["checkout", "-qb", "feature"]);
        commit_file(&rig.work, "culprit", "bad\n");
        git(&rig.work, &["checkout", "-q", "main"]);
        git(
            &rig.work,
            &["merge", "--no-ff", "-qm", "merge culprit", "feature"],
        );
        let culprit = git(&rig.work, &["rev-parse", "HEAD"]);
        let target = commit_file(&rig.work, "later", "keep\n");
        git(&rig.work, &["push", "-q", "origin", "main"]);
        let revert = rig.repo.revert_commit(&target, &culprit).unwrap();
        assert_eq!(
            git(&rig.work, &["rev-parse", &format!("{revert}^1")]),
            target
        );
        assert_eq!(
            git(&rig.work, &["show", &format!("{revert}:later")]),
            "keep"
        );
        assert_ne!(
            rig.repo
                .git
                .exit(&rig.work, &["cat-file", "-e", &format!("{revert}:culprit")])
                .unwrap()
                .0,
            0
        );
        rig.repo.push_ref("land/revert/1", &revert, None).unwrap();
        assert!(rig.repo.push_ref("land/revert/1", &revert, None).is_err());
        assert_eq!(git(&rig.bare, &["rev-parse", "refs/heads/main"]), target);
        assert_eq!(git(&rig.work, &["rev-parse", "HEAD"]), target);
    }

    #[test]
    fn shadow_fetch_does_not_change_refs_when_target_advances() {
        let mut rig = rig();
        let base = git(&rig.work, &["rev-parse", "HEAD"]);
        git(
            &rig.work,
            &["update-ref", "refs/remotes/origin/main", &base],
        );
        let before = git(
            &rig.work,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        let other = rig._dir.path().join("other");
        git(
            rig._dir.path(),
            &["clone", "-q", rig.bare.to_str().unwrap(), "other"],
        );
        git(&other, &["config", "commit.gpgsign", "false"]);
        let moved = commit_file(&other, "new", "new\n");
        git(&other, &["push", "-q", "origin", "main"]);
        assert_eq!(rig.repo.fetch_target_untracked("main").unwrap(), moved);
        assert_eq!(
            git(
                &rig.work,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            ),
            before
        );
        assert_eq!(
            git(&rig.work, &["rev-parse", "refs/remotes/origin/main"]),
            base
        );
    }

    #[test]
    fn repository_identity_preserves_the_targets_configured_remote() {
        let mut rig = rig();
        git(
            &rig.work,
            &["remote", "add", "upstream", rig.bare.to_str().unwrap()],
        );
        git(&rig.work, &["config", "branch.main.remote", "upstream"]);
        git(
            &rig.work,
            &["remote", "set-url", "origin", "missing-origin"],
        );
        let identity = rig.repo.repository_identity("main").unwrap();
        assert_eq!(identity.len(), 64);
        assert_eq!(
            rig.repo.fetch_target("main").unwrap(),
            git(&rig.work, &["rev-parse", "HEAD"])
        );
    }

    #[test]
    fn merge_tree_returns_a_tree_or_a_conflict() {
        let mut rig = rig();
        let base = git(&rig.work, &["rev-parse", "HEAD"]);
        git(&rig.work, &["checkout", "-q", "-b", "one"]);
        let one = commit_file(&rig.work, "a", "one\n");
        git(&rig.work, &["checkout", "-q", "main"]);
        git(&rig.work, &["checkout", "-q", "-b", "two"]);
        let two = commit_file(&rig.work, "b", "two\n");
        git(&rig.work, &["checkout", "-q", "-b", "three", &base]);
        let three = commit_file(&rig.work, "a", "three\n");
        let MergeTree::Tree(tree) = rig.repo.merge_tree(&one, &two).unwrap() else {
            panic!("clean merge expected");
        };
        assert_eq!(tree.len(), 40);
        assert_eq!(
            rig.repo.merge_tree(&one, &three).unwrap(),
            MergeTree::Conflict
        );
    }

    #[test]
    fn commit_tree_records_parents_in_order_and_tree_of_reads_it_back() {
        let mut rig = rig();
        let base = git(&rig.work, &["rev-parse", "HEAD"]);
        git(&rig.work, &["checkout", "-q", "-b", "side"]);
        let side = commit_file(&rig.work, "s", "s\n");
        let tree = rig.repo.tree_of(&side).unwrap();
        let commit = rig
            .repo
            .commit_tree(&tree, &[base.clone(), side.clone()], "candidate")
            .unwrap();
        assert_eq!(
            git(&rig.work, &["rev-list", "--parents", "-n1", &commit]),
            format!("{commit} {base} {side}")
        );
        assert_eq!(rig.repo.tree_of(&commit).unwrap(), tree);
        assert!(rig.repo.is_ancestor(&base, &commit).unwrap());
        assert!(!rig.repo.is_ancestor(&commit, &base).unwrap());
    }

    #[test]
    fn push_rejected_by_remote_hook_is_definite_refusal() {
        use std::os::unix::fs::PermissionsExt;
        let mut rig = rig();
        let base = rig.repo.fetch_target("main").unwrap();
        let tree = rig.repo.tree_of(&base).unwrap();
        let candidate = rig
            .repo
            .commit_tree(&tree, std::slice::from_ref(&base), "candidate")
            .unwrap();
        let hook = rig.bare.join("hooks/pre-receive");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            rig.repo.push_ref("main", &candidate, Some(&base)),
            Err(LandingError::Refused(_))
        ));
        assert_eq!(rig.repo.fetch_target("main").unwrap(), base);
    }

    #[test]
    fn push_uses_a_lease_and_refuses_a_moved_target() {
        let mut rig = rig();
        let base = rig.repo.fetch_target("main").unwrap();
        // Another writer advances the remote target.
        let other = rig._dir.path().join("other");
        git(
            rig._dir.path(),
            &["clone", "-q", rig.bare.to_str().unwrap(), "other"],
        );
        git(&other, &["config", "commit.gpgsign", "false"]);
        let moved = commit_file(&other, "o", "o\n");
        git(&other, &["push", "-q", "origin", "main"]);
        let tree = rig.repo.tree_of(&base).unwrap();
        let candidate = rig
            .repo
            .commit_tree(&tree, std::slice::from_ref(&base), "c")
            .unwrap();
        let stale = rig.repo.push_ref("main", &candidate, Some(&base));
        assert!(matches!(stale, Err(LandingError::Refused(_))), "{stale:?}");
        assert_eq!(
            git(&rig.bare, &["rev-parse", "refs/heads/main"]),
            moved,
            "the moved target is untouched"
        );
        assert_eq!(rig.repo.fetch_target("main").unwrap(), moved);
        // A new candidate ref needs an absent expectation; a different commit is refused.
        rig.repo.push_ref("land/x", &candidate, None).unwrap();
        let rival = rig
            .repo
            .commit_tree(&tree, std::slice::from_ref(&base), "rival")
            .unwrap();
        assert!(rig.repo.push_ref("land/x", &rival, None).is_err());
        assert!(rig.repo.delete_ref("land/x", &base).is_err());
        rig.repo.delete_ref("land/x", &candidate).unwrap();
        rig.repo.delete_ref("land/x", &candidate).unwrap();
        assert!(
            Command::new("git")
                .args(["rev-parse", "--verify", "-q", "refs/heads/land/x"])
                .current_dir(&rig.bare)
                .output()
                .unwrap()
                .status
                .code()
                == Some(1)
        );
    }

    #[test]
    fn refs_and_ids_are_validated_before_git_runs() {
        let mut rig = rig();
        assert!(rig.repo.push_ref("--force", &"a".repeat(40), None).is_err());
        assert!(
            rig.repo
                .push_ref("refs/heads/x", &"a".repeat(40), None)
                .is_err()
        );
        assert!(rig.repo.tree_of("HEAD").is_err());
    }

    fn definition(checks: &[&str], issuer: Option<&str>) -> GateDefinition {
        GateDefinition {
            kind: provenant_cli::domains::GateKind::Checks,
            digest: "digest".into(),
            checks: checks.iter().map(|c| (*c).into()).collect(),
            commands: Vec::new(),
            issuer: issuer.map(str::to_owned),
            executor: None,
            coverage: Vec::new(),
            timeout_seconds: 10,
            pool: None,
            env_passthrough: Vec::new(),
        }
    }
    fn candidate() -> Candidate {
        Candidate {
            base: "base".into(),
            steps: Vec::new(),
            commit: "cand".into(),
            tree: "tree".into(),
            ref_name: "land/x".into(),
        }
    }
    fn observed(checks: &[(&str, Option<&str>, GateResult)]) -> GateEvidence {
        GateEvidence {
            commit: "cand".into(),
            tree: "tree".into(),
            definition_digest: String::new(),
            checks: checks
                .iter()
                .map(|(name, issuer, result)| CheckEvidence {
                    name: (*name).into(),
                    issuer: issuer.map(str::to_owned),
                    result: result.clone(),
                })
                .collect(),
            issuer: None,
            coverage: Vec::new(),
            result: GateResult::Pending,
            pool: None,
            duration_seconds: None,
        }
    }

    #[test]
    fn gate_polls_until_required_checks_pass_and_binds_the_definition() {
        let mut forge = FakeForge::default();
        forge.check_runs.insert("cand".into(), vec![observed(&[])]);
        let mut pauses = Vec::new();
        let result = observe_checks(
            &mut forge,
            &candidate(),
            &definition(&["ci"], None),
            3,
            &mut |d| pauses.push(d),
        )
        .unwrap();
        // Never registered: the budget (10s at 3s polls) ends as Missing.
        assert_eq!(result.result, GateResult::Missing);
        assert_eq!(pauses.len(), 4);
        forge.check_runs.insert(
            "cand".into(),
            vec![observed(&[(
                "ci",
                Some("github-actions"),
                GateResult::Passed,
            )])],
        );
        let result = observe_checks(
            &mut forge,
            &candidate(),
            &definition(&["ci"], Some("github-actions")),
            3,
            &mut |_| panic!("settled evidence must not wait"),
        )
        .unwrap();
        assert_eq!(result.result, GateResult::Passed);
        assert_eq!(result.definition_digest, "digest");
        assert_eq!(result.issuer.as_deref(), Some("github-actions"));
        assert_eq!(result.pool.as_deref(), Some("github-actions"));
        let mut configured = definition(&["ci"], None);
        configured.pool = Some("hosted".into());
        let priced = observe_checks(&mut forge, &candidate(), &configured, 3, &mut |_| {}).unwrap();
        assert_eq!(priced.pool.as_deref(), Some("hosted"));
        let mut budget = provenant_cli::domains::BudgetSettings::default();
        budget.rate_micros_per_minute.insert("hosted".into(), 60);
        assert_eq!(
            super::super::budget::estimate(&budget, &[(priced.pool.unwrap(), 2)]).unwrap(),
            CostEstimate::Known { micros: 2 }
        );
    }

    #[test]
    fn mixed_check_issuers_do_not_invent_a_pool() {
        let checks = observed(&[
            ("ci", Some("first"), GateResult::Passed),
            ("ci", Some("second"), GateResult::Passed),
        ]);
        let evidence = bind_evidence(&candidate(), &definition(&["ci"], None), &checks).unwrap();
        assert_eq!(evidence.pool, None);
        assert_eq!(evidence.issuer, None);
    }

    #[test]
    fn pending_check_is_polled_until_it_passes() {
        let mut samples = std::collections::VecDeque::from([
            observed(&[("ci", Some("app"), GateResult::Pending)]),
            observed(&[("ci", Some("app"), GateResult::Passed)]),
        ]);
        let mut pauses = Vec::new();
        let result = observe_checks_with(
            &candidate(),
            &definition(&["ci"], None),
            3,
            &mut |pause| pauses.push(pause),
            &mut || Ok(vec![samples.pop_front().unwrap()]),
        )
        .unwrap();
        assert_eq!(result.result, GateResult::Passed);
        assert_eq!(result.duration_seconds, Some(3));
        assert_eq!(pauses, [Duration::from_secs(3)]);
        assert_eq!(result.pool.as_deref(), Some("app"));
    }

    #[test]
    fn gate_rejects_other_issuers_failures_and_foreign_trees() {
        let mut forge = FakeForge::default();
        forge.check_runs.insert(
            "cand".into(),
            vec![observed(&[(
                "ci",
                Some("someone-else"),
                GateResult::Passed,
            )])],
        );
        let wrong_issuer = observe_checks(
            &mut forge,
            &candidate(),
            &definition(&["ci"], Some("github-actions")),
            5,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(wrong_issuer.result, GateResult::Missing);
        forge.check_runs.insert(
            "cand".into(),
            vec![observed(&[
                ("ci", None, GateResult::Passed),
                ("lint", None, GateResult::Failed),
            ])],
        );
        let failed = observe_checks(
            &mut forge,
            &candidate(),
            &definition(&["ci", "lint"], None),
            5,
            &mut |_| panic!("a failure is final"),
        )
        .unwrap();
        assert_eq!(failed.result, GateResult::Failed);
        let mut foreign = observed(&[("ci", None, GateResult::Passed)]);
        foreign.tree = "other".into();
        forge.check_runs.insert("cand".into(), vec![foreign]);
        assert!(
            observe_checks(
                &mut forge,
                &candidate(),
                &definition(&["ci"], None),
                5,
                &mut |_| {}
            )
            .is_err()
        );
    }

    #[test]
    fn folding_ranks_failure_over_pending_over_missing_over_skipped() {
        use GateResult::*;
        assert_eq!(fold_results([Passed, Failed, Pending]), Failed);
        assert_eq!(fold_results([Passed, Pending, Missing]), Pending);
        assert_eq!(fold_results([Passed, Skipped]), Skipped);
        assert_eq!(fold_results([Passed, Passed]), Passed);
        assert_eq!(fold_results([]), Missing);
    }

    #[test]
    fn direct_push_unknown_is_not_allowed_with_a_reason() {
        let push = json!({"permissions": {"push": true}});
        let open = json!({"protected": false});
        assert!(direct_push_verdict(&push, &open, &[]).is_ok());
        assert!(direct_push_verdict(&push, &open, &[json!({"type": "non_fast_forward"})]).is_ok());
        let blocked = direct_push_verdict(&push, &open, &[json!({"type": "pull_request"})]);
        assert!(blocked.unwrap_err().contains("pull_request"));
        assert!(direct_push_verdict(&push, &json!({"protected": true}), &[]).is_err());
        assert!(direct_push_verdict(&push, &json!({}), &[]).is_err());
        assert!(direct_push_verdict(&json!({"permissions": {"push": false}}), &open, &[]).is_err());
        assert!(direct_push_verdict(&json!({}), &open, &[]).is_err());
    }

    #[test]
    fn alarm_reaches_the_chair_inbox() {
        use provenant_core::{
            CancelToken, HostId, MonoTime, ProjectId, RootInputs, TestClock, WallTime,
        };
        use provenant_fabric_runtime::{
            identity::{Address, Identity, chair_session_mailbox},
            inbox::{ControlRoute, InboxInput, InboxService},
        };
        use provenant_fabric_store::{Request, Store};
        use provenant_records::{AbsolutePath, control::LaunchKind, inbox::InteractionChannel};
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("repo"));
        inputs.state_dir = Some(base.join("state"));
        let state = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap()
            .state;
        let host = HostId::from_bytes([1; 16]);
        let identity = Identity {
            project: ProjectId::from_bytes([2; 16]),
            host,
            inbox: Address::new(host, "chair").unwrap(),
            client: "test".into(),
            cwd: AbsolutePath::new(&base).unwrap(),
            common_dir: AbsolutePath::new(&base).unwrap(),
            federation: None,
        };
        let clock = TestClock::new(
            WallTime::from_micros(2_000_000_000_000_000).unwrap(),
            MonoTime::from_micros(0),
        );
        let cancel = CancelToken::new();
        let lookup = |_: &InboxInput, _: &Request<'_>| {
            Ok(ControlRoute {
                provider: None,
                launch: LaunchKind::Chair,
                registered: true,
                target: "chair-a".into(),
            })
        };
        let alarm = Alarm {
            code: "post_merge_failed".into(),
            message: "check Build failed".into(),
        };
        deliver_alarm(
            Some("chair-a"),
            &alarm_text(&alarm),
            &mut |session, text| {
                InboxService::open(&state, &identity, &clock, &cancel)
                    .unwrap()
                    .with_control_lookup(&lookup)
                    .execute(
                        InboxInput::ChairMessage {
                            extra: Default::default(),
                            session: session.into(),
                            text: text.into(),
                            channel: InteractionChannel::Cli,
                            operation_id: None,
                        },
                        &cancel,
                    )
                    .unwrap();
                Ok(())
            },
        )
        .unwrap();
        let request = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let mail = Store::open(&state, &request)
            .unwrap()
            .read_mail(
                identity.project,
                chair_session_mailbox(identity.project, "chair-a"),
                Some(0),
                50,
                &request,
            )
            .unwrap();
        assert_eq!(mail.items.len(), 1);
        assert!(mail.items[0].message.body.contains("check Build failed"));
    }

    #[test]
    fn alarm_delivery_targets_chair_and_propagates_failure() {
        let mut delivered = Vec::new();
        deliver_alarm(None, "alarm", &mut |session, text| {
            delivered.push((session.to_owned(), text.to_owned()));
            Ok(())
        })
        .unwrap();
        assert!(delivered.is_empty());
        deliver_alarm(Some("chair"), "alarm", &mut |session, text| {
            delivered.push((session.to_owned(), text.to_owned()));
            Ok(())
        })
        .unwrap();
        assert_eq!(delivered, [("chair".into(), "alarm".into())]);
        assert!(
            deliver_alarm(Some("chair"), "alarm", &mut |_, _| Err(
                LandingError::Failed("delivery failed".into())
            ))
            .is_err()
        );
    }

    #[test]
    fn alarm_text_names_code_and_is_bounded() {
        let text = alarm_text(&Alarm {
            code: "TREE_MISMATCH".into(),
            message: "x".repeat(9000),
        });
        assert!(text.starts_with("landing alarm TREE_MISMATCH: "));
        assert_eq!(text.chars().count(), 4000);
    }
}
