//! Real forge, git, gate and alarm adapters for the landing driver.
use super::{
    LandingError,
    model::*,
    ports::{
        AlarmSink, Approval, Forge, GateRunner, MergeTree, PullRequestInfo, PullRequestState, Repo,
        ReviewRecord, ReviewRecordSource, Reviews, StackMember, StackMembership,
    },
};
use crate::pr::{
    RestPull, TimedOut, execute_with,
    gh::REST_API_VERSION_HEADER,
    git::Git,
    git::plain_ref,
    merge_with,
    observer::{Check, ObserveError, Observer},
    wait,
};
use provenant_cli::domains::MergeMethod;
use provenant_os::exec::Termination;
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

/// The side-effect adapters used by the landing driver.
pub type Adapters<'a> = (
    Box<dyn Forge>,
    Box<dyn Repo + 'a>,
    Box<dyn GateRunner>,
    Box<dyn AlarmSink>,
);

/// Construct the real forge, repository, gate and alarm adapters for a project.
pub(super) fn connect<'a>(
    project: &Path,
    state: &Path,
    globals: &provenant_cli::Globals,
    workers: &'a dyn provenant_confinement::worktrees::WorkerObserver,
    tracking: std::collections::BTreeMap<u64, crate::pr::land::BatchTracking>,
) -> Result<Adapters<'a>, LandingError> {
    Ok((
        Box::new(GhForge::new(project, state)),
        Box::new(GitRepo::with_follow_up(
            project, state, globals, workers, tracking,
        )?),
        Box::new(CheckGate::new(project, state)),
        Box::new(ChairAlarms::new(project)),
    ))
}

fn failed(error: Box<dyn std::error::Error>) -> LandingError {
    if error
        .downcast_ref::<wait::Failure>()
        .is_some_and(|failure| failure.code == "RATE_LIMITED")
    {
        LandingError::RateLimited(error.to_string())
    } else {
        LandingError::Failed(error.to_string())
    }
}
/// A lost or slow mutation leaves the outcome unknown rather than failed.
fn mutation_failed(error: Box<dyn std::error::Error>) -> LandingError {
    if error.is::<TimedOut>() {
        LandingError::PublicationUnknown
    } else if error
        .downcast_ref::<wait::Failure>()
        .is_some_and(|failure| matches!(failure.code, "HEAD_MOVED" | "MERGE_NOT_ALLOWED"))
        || ["HTTP 405", "HTTP 409", "HTTP 422"]
            .iter()
            .any(|code| error.to_string().contains(code))
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

/// Interpret the documented asynchronous merge result. Unknown shapes must
/// stay uncertain: the request may already have changed the forge.
fn async_merge_result(value: &serde_json::Value) -> Result<Option<String>, LandingError> {
    match value["status"].as_str() {
        Some("merged") => {
            let sha = value["details"]["sha"]
                .as_str()
                .ok_or(LandingError::PublicationUnknown)?;
            object_id(sha).map_err(|_| LandingError::PublicationUnknown)?;
            Ok(Some(sha.to_owned()))
        }
        Some("pending") => Ok(None),
        Some("failed") => Err(LandingError::Refused(
            value["details"]["message"]
                .as_str()
                .unwrap_or("asynchronous merge failed")
                .to_owned(),
        )),
        // Enqueued is not merged; its final outcome must be reconciled.
        _ => Err(LandingError::PublicationUnknown),
    }
}

fn reconcile_revert_creation(
    result: Result<Vec<u8>, LandingError>,
    lookup: impl FnOnce() -> Result<Option<u64>, LandingError>,
) -> Result<u64, LandingError> {
    let parsed = result.and_then(|bytes| {
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
            LandingError::Failed(format!("invalid revert pull request response: {e}"))
        })?;
        value["number"].as_u64().ok_or_else(|| {
            LandingError::Failed("revert pull request response has no number".into())
        })
    });
    match parsed {
        Ok(number) => Ok(number),
        Err(error) => lookup()?.ok_or(error),
    }
}

/// The gate's reading of observed checks, and their folded result. A neutral
/// conclusion is distinct from a skip; anything not passed, pending or
/// skipped has failed.
pub(crate) fn gate_checks(runs: Vec<Check>) -> (Vec<CheckEvidence>, GateResult) {
    let checks: Vec<_> = runs
        .into_iter()
        .map(|run| CheckEvidence {
            name: run.name,
            issuer: run.app_slug,
            result: match run.bucket.as_str() {
                "pass" => GateResult::Passed,
                "pending" => GateResult::Pending,
                "skipping" if run.conclusion.as_deref() == Some("neutral") => GateResult::Neutral,
                "skipping" => GateResult::Skipped,
                _ => GateResult::Failed,
            },
        })
        .collect();
    let result = fold_results(checks.iter().map(|check| check.result.clone()));
    (checks, result)
}

// ----- Forge -------------------------------------------------------------

/// GitHub through the `gh` REST edge, with the shared pacing and rate-limit retry.
pub struct GhForge {
    project: PathBuf,
    /// The state root holding the shared CI observer cache.
    state: PathBuf,
    /// `owner/name`, read once.
    repository: Option<String>,
    /// Adapter-owned cadence from the shared observer; caller settings remain
    /// the fallback for forge implementations without an observer.
    observation_interval: Option<Duration>,
    /// Explicit resolved landing cadence wins over the observer's adaptive value.
    poll_interval_override: Option<Duration>,
    budget: Duration,
    deadline: Option<std::time::Instant>,
}
impl GhForge {
    pub fn new(project: &Path, state: &Path) -> Self {
        Self {
            project: project.to_owned(),
            state: state.to_owned(),
            repository: None,
            observation_interval: None,
            poll_interval_override: None,
            budget: Duration::from_secs(120),
            deadline: None,
        }
    }
    #[allow(clippy::disallowed_methods)]
    fn reader(&self) -> wait::Reader<'_> {
        wait::Reader {
            project: &self.project,
            deadline: self
                .deadline
                .unwrap_or_else(|| std::time::Instant::now() + self.budget),
            retry_command: "provenant land".into(),
        }
    }
    fn poll_interval(&self) -> Option<Duration> {
        self.poll_interval_override.or(self.observation_interval)
    }
    fn repository_name(&mut self) -> Result<String, LandingError> {
        if let Some(repository) = &self.repository {
            return Ok(repository.clone());
        }
        let budget = self
            .reader()
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        let repository =
            crate::pr::observer::repository_for_project(&self.state, &self.project, budget)
                .map_err(|error| match error {
                    ObserveError::RateLimited { .. } => {
                        LandingError::RateLimited(error.to_string())
                    }
                    _ => LandingError::Failed(error.to_string()),
                })?;
        self.repository = Some(repository.clone());
        Ok(repository)
    }
    fn observed_resource(
        &mut self,
        path: &str,
        budget: Duration,
    ) -> Result<serde_json::Value, LandingError> {
        let repository = self.repository_name()?;
        let mut observer = Observer::open(&self.state, &self.project, &repository, budget)
            .map_err(|error| match error {
                ObserveError::RateLimited { .. } => LandingError::RateLimited(error.to_string()),
                _ => LandingError::Failed(error.to_string()),
            })?;
        let resource =
            observer
                .resource(path, Duration::ZERO, budget)
                .map_err(|error| match error {
                    ObserveError::RateLimited { .. } => {
                        LandingError::RateLimited(error.to_string())
                    }
                    _ => LandingError::Failed(error.to_string()),
                })?;
        self.observation_interval = Some(observer.fast_interval());
        Ok(resource)
    }
    fn pull_request_with_budget(
        &mut self,
        number: u64,
        budget: Duration,
    ) -> Result<PullRequestInfo, LandingError> {
        let pull: RestPull =
            serde_json::from_value(self.observed_resource(&format!("pulls/{number}"), budget)?)
                .map_err(|error| {
                    LandingError::Failed(format!(
                        "GitHub returned invalid pull request JSON: {error}"
                    ))
                })?;
        Ok(pull_info(number, pull))
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

    #[allow(clippy::disallowed_methods, reason = "bounded forge polling")]
    fn merge_stack(
        &mut self,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<String, LandingError> {
        object_id(head)?;
        let pull = self.pull_request(number)?;
        let observation_interval = self
            .poll_interval()
            .ok_or_else(|| LandingError::Failed("forge observation cadence unavailable".into()))?;
        let membership = pull.stack.ok_or(LandingError::PublicationUnknown)?;
        let members = self.stack_members(membership.number)?;
        let position = members
            .iter()
            .position(|member| member.number == number)
            .ok_or(LandingError::PublicationUnknown)?;
        if let Some(lower) = members[..position]
            .iter()
            .find(|member| member.state != PullRequestState::Merged)
        {
            return Err(LandingError::Refused(format!(
                "pull request #{number} has unmerged lower pull request #{}",
                lower.number
            )));
        }
        let upper = members[position + 1..]
            .iter()
            .find(|member| member.state == PullRequestState::Open)
            .map(|member| member.number);
        let method = match method {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        };
        let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}/merge-async");
        let sha = format!("sha={head}");
        let merge_method = format!("merge_method={method}");
        let deadline = std::time::Instant::now() + self.budget;
        let reader = self.reader();
        let output = reader
            .request(
                &[
                    "gh",
                    "api",
                    "--method",
                    "PUT",
                    &endpoint,
                    "-H",
                    REST_API_VERSION_HEADER,
                    "-f",
                    &sha,
                    "-f",
                    &merge_method,
                    "-f",
                    "merge_action=direct_merge",
                ],
                true,
            )
            .map_err(|_| LandingError::PublicationUnknown)?;
        if output.termination != Termination::Exit(0) {
            // In particular, 409 can name an existing in-flight request whose
            // options differ from ours. Recovery must observe the forge first.
            return Err(LandingError::PublicationUnknown);
        }
        let mut result: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)
            .map_err(|_| LandingError::PublicationUnknown)?;
        loop {
            if let Some(sha) = async_merge_result(&result)? {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Err(LandingError::PublicationUnknown);
                }
                let observed = self
                    .pull_request_with_budget(number, left)
                    .map_err(|_| LandingError::PublicationUnknown)?;
                if observed.state == PullRequestState::Merged
                    && observed.head == head
                    && observed.merged_commit.as_deref() == Some(&sha)
                {
                    if let Some(upper) = upper {
                        loop {
                            let left =
                                deadline.saturating_duration_since(std::time::Instant::now());
                            if left.is_zero() {
                                return Err(LandingError::PublicationUnknown);
                            }
                            let next = self
                                .pull_request_with_budget(upper, left)
                                .map_err(|_| LandingError::PublicationUnknown)?;
                            if next.state != PullRequestState::Open
                                || next.base_branch == membership.base_branch
                            {
                                break;
                            }
                            std::thread::sleep(observation_interval.min(left));
                        }
                    }
                    return Ok(sha);
                }
                return Err(LandingError::PublicationUnknown);
            }
            let details = &result["details"];
            if details["expected_head_sha"].as_str() != Some(head)
                || details["merge_method"].as_str() != Some(method)
                || details["merge_action"].as_str() != Some("direct_merge")
            {
                return Err(LandingError::PublicationUnknown);
            }
            let uuid = details["uuid"]
                .as_str()
                .ok_or(LandingError::PublicationUnknown)?;
            if uuid.len() != 36 || !uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
                return Err(LandingError::PublicationUnknown);
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(LandingError::PublicationUnknown);
            }
            std::thread::sleep(observation_interval.min(left));
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(LandingError::PublicationUnknown);
            }
            result = self
                .observed_resource(&format!("pulls/{number}/merge-async/{uuid}"), left)
                .map_err(|_| LandingError::PublicationUnknown)?;
        }
    }
}

fn pull_info(number: u64, pull: RestPull) -> PullRequestInfo {
    let merged = pull.merged_at.is_some();
    PullRequestInfo {
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
        stack: pull.stack.map(|stack| StackMembership {
            number: stack.number,
            size: stack.size,
            position: stack.position,
            base_branch: stack.base.branch,
        }),
    }
}

fn stack_members(value: &serde_json::Value) -> Result<Vec<StackMember>, LandingError> {
    let members = value["pull_requests"]
        .as_array()
        .ok_or_else(|| LandingError::Failed("stack response has no pull requests".into()))?;
    let mut seen = std::collections::BTreeSet::new();
    members
        .iter()
        .map(|member| {
            let number = member["number"]
                .as_u64()
                .filter(|number| *number != 0 && seen.insert(*number))
                .ok_or_else(|| {
                    LandingError::Failed("stack has invalid pull request number".into())
                })?;
            let state = match (member["state"].as_str(), member.get("merged_at")) {
                (Some("closed"), Some(serde_json::Value::String(date))) if !date.is_empty() => {
                    PullRequestState::Merged
                }
                (Some("closed"), Some(serde_json::Value::Null)) => PullRequestState::Closed,
                (Some("open"), Some(serde_json::Value::Null)) => PullRequestState::Open,
                _ => {
                    return Err(LandingError::Failed(
                        "stack has invalid pull request state".into(),
                    ));
                }
            };
            Ok(StackMember { number, state })
        })
        .collect()
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
    fn capabilities(
        &mut self,
        target: &str,
    ) -> Result<provenant_records::landing::Capabilities, LandingError> {
        use provenant_records::landing::{Capabilities, CapabilityState, ForgeCapability};
        let repository = self.repository_identity()?;
        let metadata = self.json("repos/{owner}/{repo}")?;
        let mut values = std::collections::BTreeMap::new();
        if let Some(allowed) = metadata["allow_auto_merge"].as_bool() {
            values.insert(
                ForgeCapability::AutoMerge,
                if allowed {
                    CapabilityState::Available
                } else {
                    CapabilityState::Unavailable
                },
            );
        }
        // Rules are specific to the root target, including names with slashes.
        let encoded = encode_branch(target);
        if let Ok(rules) = self.json(&format!(
            "repos/{{owner}}/{{repo}}/rules/branches/{encoded}"
        )) && let Some(rules) = rules.as_array()
        {
            values.insert(
                ForgeCapability::MergeQueue,
                if rules.iter().any(|rule| rule["type"] == "merge_queue") {
                    CapabilityState::Available
                } else {
                    CapabilityState::Unavailable
                },
            );
        }
        // A successful, typed stack listing is positive evidence for this repository.
        // A forbidden or unavailable endpoint remains unknown, never assumed absent.
        if let Ok(stacks) = self.json("repos/{owner}/{repo}/stacks")
            && stacks["stacks"].is_array()
        {
            for capability in [ForgeCapability::AsyncMerge, ForgeCapability::Restack] {
                values.insert(capability, CapabilityState::Available);
            }
        }
        Ok(Capabilities {
            repository: Some(repository),
            values,
        })
    }
    fn stack_members(&mut self, number: u64) -> Result<Vec<StackMember>, LandingError> {
        let endpoint = format!("repos/{{owner}}/{{repo}}/stacks/{number}");
        let bytes = self.get(&[&endpoint, "-H", REST_API_VERSION_HEADER])?;
        let response: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            LandingError::Failed(format!("GitHub returned invalid stack JSON: {error}"))
        })?;
        if response["number"].as_u64() != Some(number) {
            return Err(LandingError::Failed(
                "stack response number does not match request".into(),
            ));
        }
        stack_members(&response)
    }
    fn set_project_path(&mut self, path: &Path) {
        self.project = path.to_owned();
        self.repository = None;
        self.observation_interval = None;
    }
    fn set_poll_interval_override(&mut self, interval: Option<Duration>) {
        self.poll_interval_override = interval;
    }
    fn repository_identity(&mut self) -> Result<String, LandingError> {
        let repository = self.json("repos/{owner}/{repo}")?;
        let url = repository["html_url"]
            .as_str()
            .ok_or_else(|| LandingError::Failed("repository has no canonical URL".into()))?;
        let name = repository["full_name"]
            .as_str()
            .ok_or_else(|| LandingError::Failed("repository has no canonical name".into()))?;
        let host = url
            .split('/')
            .nth(2)
            .filter(|host| !host.is_empty())
            .ok_or_else(|| LandingError::Failed("repository has invalid canonical URL".into()))?;
        // Its first 16 bytes are the repository ID a receipt subject records.
        Ok(
            provenant_records::workflow::forge_repository_digest(host, name)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    fn pull_request(&mut self, number: u64) -> Result<PullRequestInfo, LandingError> {
        self.pull_request_with_budget(number, self.budget)
    }
    fn actor(&mut self) -> Result<String, LandingError> {
        self.json("user")?["login"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| LandingError::Failed("the forge reported no login".into()))
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
    fn observation_interval(&self) -> Option<Duration> {
        self.poll_interval()
    }
    fn checks(&mut self, commit: &str) -> Result<Vec<GateEvidence>, LandingError> {
        let commit = object_id(commit)?;
        let repository = self.repository_name()?;
        let deadline = self.reader().deadline;
        #[allow(clippy::disallowed_methods, reason = "one forge observation deadline")]
        let remaining = || deadline.saturating_duration_since(std::time::Instant::now());
        let mut observer = Observer::open(&self.state, &self.project, &repository, remaining())
            .map_err(|error| match error {
                ObserveError::RateLimited { .. } => LandingError::RateLimited(error.to_string()),
                _ => LandingError::CiUnavailable(error.to_string()),
            })?;
        self.observation_interval = Some(observer.fast_interval());
        let runs = observer
            .checks(commit, Duration::ZERO, remaining())
            .map_err(|error| match error {
                ObserveError::RateLimited { .. } => LandingError::RateLimited(error.to_string()),
                _ => LandingError::CiUnavailable(error.to_string()),
            })?;
        self.observation_interval = Some(observer.next_poll_interval());
        let commit_resource = observer
            .resource(
                &format!("git/commits/{commit}"),
                Duration::ZERO,
                remaining(),
            )
            .map_err(|error| match error {
                ObserveError::RateLimited { .. } => LandingError::RateLimited(error.to_string()),
                _ => LandingError::CiUnavailable(error.to_string()),
            })?;
        let tree = commit_resource["tree"]["sha"]
            .as_str()
            .ok_or_else(|| LandingError::Failed("commit has no tree".into()))?
            .to_owned();
        let (checks, result) = gate_checks(runs);
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
    #[allow(clippy::disallowed_methods)]
    fn checks_with_timeout(
        &mut self,
        commit: &str,
        timeout: Duration,
    ) -> Result<Vec<GateEvidence>, LandingError> {
        self.deadline = Some(std::time::Instant::now() + timeout);
        let result = self.checks(commit);
        self.deadline = None;
        result
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
    fn merge_stack(
        &mut self,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<String, LandingError> {
        GhForge::merge_stack(self, number, head, method)
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
    fn find_revert(
        &mut self,
        number: u64,
        branch: &str,
        target: &str,
    ) -> Result<Option<u64>, LandingError> {
        let title = format!("Revert pull request #{number}");
        let body = format!("Reverts pull request #{number} after post-merge check failure:");
        let repository = self.json("repos/{owner}/{repo}")?;
        let owner = repository["owner"]["login"]
            .as_str()
            .ok_or_else(|| LandingError::Failed("repository has no owner".into()))?;
        let endpoint = format!(
            "repos/{{owner}}/{{repo}}/pulls?state=open&head={}:{}&base={}&per_page=100",
            encode_branch(owner),
            encode_branch(branch),
            encode_branch(target)
        );
        let pulls = self.pages(&endpoint)?;
        Ok(pulls
            .iter()
            .find(|pr| {
                pr["head"]["ref"].as_str() == Some(branch)
                    && pr["base"]["ref"].as_str() == Some(target)
                    && pr["head"]["repo"]["full_name"] == repository["full_name"]
                    && pr["title"].as_str() == Some(&title)
                    && pr["body"]
                        .as_str()
                        .is_some_and(|value| value.starts_with(&body))
            })
            .and_then(|pr| pr["number"].as_u64()))
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
        if let Some(existing) = self.find_revert(number, branch, target)? {
            return Ok(existing);
        }
        let args = [
            "gh",
            "api",
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
        ];
        let result = execute_with(&self.project, &args, self.budget, false, None);
        let response = match result {
            Ok(output) if output.termination == Termination::Exit(0) => Ok(output.stdout.bytes),
            other => Err(LandingError::Failed(format!(
                "revert pull request creation failed: {other:?}"
            ))),
        };
        reconcile_revert_creation(response, || self.find_revert(number, branch, target))
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandResult {
    Landed,
    Refused,
    Unknown,
}

fn command_result(
    command: &provenant_records::landing::RepositoryCommand,
    termination: Termination,
    timed_out: bool,
) -> CommandResult {
    if timed_out {
        return CommandResult::Unknown;
    }
    match termination {
        Termination::Exit(0) => CommandResult::Landed,
        Termination::Exit(code) if code == command.unknown_exit_code => CommandResult::Unknown,
        Termination::Exit(_) => CommandResult::Refused,
        _ => CommandResult::Unknown,
    }
}

// ----- Repo --------------------------------------------------------------

/// Git plumbing in the project checkout. Refs live on the remote the target tracks.
pub struct GitRepo<'a> {
    project: PathBuf,
    git: Git,
    remote: Option<String>,
    local: Option<LocalFollowUp<'a>>,
}
struct LocalFollowUp<'a> {
    globals: provenant_cli::Globals,
    workers: &'a dyn provenant_confinement::worktrees::WorkerObserver,
    reports: std::collections::BTreeMap<u64, serde_json::Value>,
    tracking: std::collections::BTreeMap<u64, crate::pr::land::BatchTracking>,
}
impl<'a> GitRepo<'a> {
    pub fn new(project: &Path, host_state: &Path) -> Result<Self, LandingError> {
        Ok(Self {
            project: project.to_owned(),
            git: Git::new(host_state).map_err(failed)?,
            remote: None,
            local: None,
        })
    }
    fn with_follow_up(
        project: &Path,
        host_state: &Path,
        globals: &provenant_cli::Globals,
        workers: &'a dyn provenant_confinement::worktrees::WorkerObserver,
        tracking: std::collections::BTreeMap<u64, crate::pr::land::BatchTracking>,
    ) -> Result<Self, LandingError> {
        let mut repo = Self::new(project, host_state)?;
        repo.local = Some(LocalFollowUp {
            globals: provenant_cli::Globals {
                json: globals.json,
                full: globals.full,
                state_dir: globals.state_dir.clone(),
                project: globals.project.clone(),
            },
            workers,
            reports: Default::default(),
            tracking,
        });
        Ok(repo)
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
        self.ok(&[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--quiet",
            &remote,
            commit,
        ])
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
                if ["stale info", "non-fast-forward", "fetch first"]
                    .iter()
                    .any(|reason| output.contains(reason) || error.contains(reason)) =>
            {
                Err(LandingError::TargetMoved)
            }
            Ok((_, output, error))
                if output.contains("[remote rejected]") || error.contains("[remote rejected]") =>
            {
                Err(LandingError::Refused(format!("git push refused: {error}")))
            }
            Ok((_, _, error)) => Err(LandingError::Failed(format!("git push failed: {error}"))),
            Err(error) => Err(mutation_failed(error)),
        }
    }
}

impl Repo for GitRepo<'_> {
    fn publish_command(
        &mut self,
        command: &provenant_records::landing::RepositoryCommand,
        attempt: &PendingAttempt,
    ) -> Result<PublishResult, LandingError> {
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor};
        if !command.valid() {
            return Err(LandingError::Refused("invalid repository publisher".into()));
        }
        let input = serde_json::to_vec(attempt)
            .map_err(|error| LandingError::Refused(format!("encode command intent: {error}")))?;
        let argv: Vec<std::ffi::OsString> = command.argv.iter().map(Into::into).collect();
        let mut options = ExecOptions::new(
            &argv,
            &self.project,
            Duration::from_secs(command.timeout_seconds),
        );
        options.stdin = Some(&input);
        options.cleanup_process_group = true;
        let executor = SystemExecutor::new(1).map_err(|error| {
            LandingError::Refused(format!("prepare repository publisher: {error}"))
        })?;
        let outcome = executor
            .execute(options)
            .map_err(|_| LandingError::PublicationUnknown)?;
        // Publisher output is diagnostic; only the declared exit protocol is evidence.
        let _ = std::io::stderr().write_all(&outcome.stdout.bytes);
        let _ = std::io::stderr().write_all(&outcome.stderr.bytes);
        match command_result(command, outcome.termination, outcome.timed_out) {
            CommandResult::Landed => Ok(PublishResult {
                landed: attempt
                    .candidate
                    .steps
                    .iter()
                    .map(|step| step.commit.clone())
                    .collect(),
                alarms: Vec::new(),
            }),
            CommandResult::Unknown => Err(LandingError::PublicationUnknown),
            CommandResult::Refused => Err(LandingError::Refused(
                "repository publisher refused the intent".into(),
            )),
        }
    }
    fn project_path(&self) -> Option<&Path> {
        self.local.as_ref().map(|_| self.project.as_path())
    }
    fn follow_local(&mut self, pr: &EligiblePr) -> Result<bool, LandingError> {
        if let Some(local) = &mut self.local {
            let head = provenant_core::GitOid::parse(&pr.head)
                .map_err(|error| LandingError::Failed(error.to_string()))?;
            self.git.invalidate_repositories();
            let result = crate::pr::land::batch_follow_up(
                &self.git,
                &self.project,
                pr.number,
                head,
                &mut local.globals,
                local.workers,
                local.tracking.get(&pr.number),
            );
            self.git.invalidate_repositories();
            if let Some(project) = &local.globals.project {
                self.project = project.clone();
            }
            let steps = match result {
                Ok(steps) => steps,
                Err(error) => {
                    let message = error.to_string();
                    local.reports.insert(
                        pr.number,
                        serde_json::json!({"head": pr.head, "steps": [], "error": message}),
                    );
                    return Err(LandingError::LocalFollowUp(message));
                }
            };
            let refused = steps
                .as_array()
                .is_some_and(|steps| steps.iter().any(|step| step["status"] == "refused"));
            local.reports.insert(
                pr.number,
                serde_json::json!({"head": pr.head, "steps": steps, "cleanup_pending": refused, "reason": if refused { "cleanup_pending" } else { "landed" }}),
            );
            return Ok(!refused);
        }
        Ok(true)
    }

    fn take_local_reports(&mut self) -> std::collections::BTreeMap<u64, serde_json::Value> {
        self.local
            .as_mut()
            .map(|local| std::mem::take(&mut local.reports))
            .unwrap_or_default()
    }
    fn fetch_target(&mut self, branch: &str) -> Result<String, LandingError> {
        let full = Self::branch_ref(branch)?;
        let remote = self.remote(branch)?;
        let output = self.ok(&["ls-remote", "--refs", &remote, &full])?;
        let (commit, reference) = output
            .split_once('\t')
            .ok_or_else(|| LandingError::Failed(format!("target ref not found: {full}")))?;
        if reference != full {
            return Err(LandingError::Failed("unexpected target ref".into()));
        }
        let commit = object_id(commit)?.to_owned();
        self.ok(&[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--quiet",
            &remote,
            &commit,
        ])?;
        Ok(commit)
    }
    fn fetch_target_untracked(&mut self, branch: &str) -> Result<String, LandingError> {
        self.fetch_target(branch)
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
        self.ok(&args).map_err(|error| match error {
            LandingError::Failed(message)
                if message.contains("identity unknown")
                    || message.contains("unable to auto-detect email address") =>
            {
                LandingError::Failed(format!(
                    "{message}; configure Git user.name and user.email for candidate commits"
                ))
            }
            other => other,
        })
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
    fn merge_base(&mut self, left: &str, right: &str) -> Result<Option<String>, LandingError> {
        self.ensure(left)?;
        self.ensure(right)?;
        let (code, out, error) = self.exit(&["merge-base", object_id(left)?, object_id(right)?])?;
        match code {
            0 => object_id(out.trim()).map(|base| Some(base.to_owned())),
            1 => Ok(None),
            _ => Err(LandingError::Failed(format!(
                "git merge-base failed: {error}"
            ))),
        }
    }
    fn changed_paths(&mut self, from: &str, to: &str) -> Result<Vec<String>, LandingError> {
        let (code, out, error) = self.exit(&[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            object_id(from)?,
            object_id(to)?,
        ])?;
        if code != 0 {
            return Err(LandingError::Failed(format!("git diff failed: {error}")));
        }
        Ok(out.split_terminator('\0').map(str::to_owned).collect())
    }
    fn tree_of(&mut self, commit: &str) -> Result<String, LandingError> {
        self.ensure(commit)?;
        let canonical = object_id(commit)?.to_ascii_lowercase();
        let commit = provenant_core::GitOid::parse(&canonical)
            .map_err(|error| LandingError::Failed(error.to_string()))?;
        Ok(self
            .git
            .commit_tree(&self.project, commit)
            .map_err(failed)?
            .to_string())
    }
}

// ----- ReviewRecords -----------------------------------------------------

/// Typed review-lane settlements in the registered project's graph journal.
pub struct ProvenantReviews {
    root: provenant_core::StateRoot,
}
impl ProvenantReviews {
    pub fn new(root: provenant_core::StateRoot) -> Self {
        Self { root }
    }
}
impl ReviewRecordSource for ProvenantReviews {
    fn reviews(
        &self,
        scope: &provenant_records::landing::ReviewScope,
        _number: u64,
        head: &str,
    ) -> Result<Reviews, LandingError> {
        let Some(project) = scope.project else {
            return Ok(Reviews {
                unusable: vec!["provenant_review needs a registered project".to_owned()],
                ..Reviews::default()
            });
        };
        let head = head
            .parse::<provenant_core::GitOid>()
            .map_err(|error| LandingError::Failed(error.to_string()))?;
        let found = provenant_fabric_runtime::graph::landing_reviews(&self.root, project, head)
            .map_err(|error| LandingError::Failed(format!("read review records: {error}")))?;
        Ok(Reviews {
            records: found
                .into_iter()
                .map(|record| ReviewRecord {
                    reference: record.attempt_id.to_string(),
                    reviewer: record.attempt_id.to_string(),
                    head: record.head.to_string(),
                    verdict: record.verdict,
                    independent: record.independent,
                })
                .collect(),
            unusable: Vec::new(),
        })
    }
}

// ----- GateRunner --------------------------------------------------------

/// Observes forge check runs on the exact candidate, or runs the command plan.
pub struct CheckGate {
    project: PathBuf,
    forge: GhForge,
}
impl CheckGate {
    pub fn new(project: &Path, state: &Path) -> Self {
        Self {
            project: project.to_owned(),
            forge: GhForge::new(project, state),
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
        let checks = forge.checks(&candidate.commit);
        let next_interval = forge.observation_interval();
        CheckObservation {
            checks,
            next_interval,
        }
    })
}

struct CheckObservation {
    checks: Result<Vec<GateEvidence>, LandingError>,
    next_interval: Option<Duration>,
}

fn observe_checks_with(
    candidate: &Candidate,
    definition: &GateDefinition,
    poll_seconds: u64,
    sleep: &mut dyn FnMut(Duration),
    read: &mut dyn FnMut() -> CheckObservation,
) -> Result<GateEvidence, LandingError> {
    let fallback = Duration::from_secs(poll_seconds);
    let timeout = Duration::from_secs(definition.timeout_seconds);
    let mut elapsed = Duration::ZERO;
    loop {
        let observation = read();
        let poll = observation.next_interval.unwrap_or(fallback);
        let observed = match observation.checks {
            // A rate limit skips this poll; the bound still ends the wait.
            Err(LandingError::RateLimited(message)) if elapsed < timeout => {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: candidate {} check read skipped: {message}",
                    candidate.commit
                );
                let pause = poll.min(timeout.saturating_sub(elapsed));
                sleep(pause);
                elapsed += pause;
                continue;
            }
            observed => observed?,
        };
        let observed = observed
            .into_iter()
            .next()
            .ok_or_else(|| LandingError::Failed("forge returned no check evidence".into()))?;
        let mut evidence = bind_evidence(candidate, definition, &observed)?;
        evidence.duration_seconds = Some(elapsed.as_secs());
        let settled = !matches!(evidence.result, GateResult::Pending | GateResult::Missing);
        if settled || elapsed >= timeout {
            return Ok(evidence);
        }
        let pause = poll.min(timeout.saturating_sub(elapsed));
        sleep(pause);
        elapsed += pause;
    }
}
impl GateRunner for CheckGate {
    fn set_project_path(&mut self, path: &Path) {
        self.project = path.to_owned();
        self.forge.set_project_path(path);
    }
    fn set_poll_interval_override(&mut self, interval: Option<Duration>) {
        self.forge.set_poll_interval_override(interval);
    }
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
    fn set_project_path(&mut self, path: &Path) {
        self.project = path.to_owned();
    }
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
    #[test]
    fn repository_command_exit_protocol_preserves_uncertainty() {
        use provenant_records::landing::RepositoryCommand;
        for unknown_exit_code in [17, 75, 255] {
            let command = RepositoryCommand {
                argv: vec!["publisher".into()],
                unknown_exit_code,
                timeout_seconds: 30,
            };
            for code in [0, 1, unknown_exit_code] {
                let result = super::command_result(&command, Termination::Exit(code), false);
                assert_eq!(
                    result,
                    if code == 0 {
                        super::CommandResult::Landed
                    } else if code == unknown_exit_code {
                        super::CommandResult::Unknown
                    } else {
                        super::CommandResult::Refused
                    }
                );
                assert_eq!(
                    super::command_result(&command, Termination::Exit(code), true),
                    super::CommandResult::Unknown
                );
            }
            assert_eq!(
                super::command_result(&command, Termination::Signal(9), false),
                super::CommandResult::Unknown
            );
        }
    }

    use super::*;

    #[test]
    fn explicit_poll_override_precedes_adaptive_interval_and_gate_forwards_it() {
        let adaptive = Duration::from_secs(5);
        let fixed = Duration::from_secs(30);
        let mut forge = GhForge::new(Path::new("project"), Path::new("state"));
        forge.observation_interval = Some(adaptive);
        assert_eq!(Forge::observation_interval(&forge), Some(adaptive));
        Forge::set_poll_interval_override(&mut forge, Some(fixed));
        assert_eq!(Forge::observation_interval(&forge), Some(fixed));
        Forge::set_project_path(&mut forge, Path::new("candidate-worktree"));
        assert_eq!(Forge::observation_interval(&forge), Some(fixed));
        forge.observation_interval = Some(adaptive);
        Forge::set_poll_interval_override(&mut forge, None);
        assert_eq!(Forge::observation_interval(&forge), Some(adaptive));

        let mut gate = CheckGate::new(Path::new("project"), Path::new("state"));
        gate.forge.observation_interval = Some(adaptive);
        GateRunner::set_poll_interval_override(&mut gate, Some(fixed));
        assert_eq!(Forge::observation_interval(&gate.forge), Some(fixed));
        GateRunner::set_project_path(&mut gate, Path::new("candidate-worktree"));
        assert_eq!(Forge::observation_interval(&gate.forge), Some(fixed));
        gate.forge.observation_interval = Some(adaptive);
        GateRunner::set_poll_interval_override(&mut gate, None);
        assert_eq!(Forge::observation_interval(&gate.forge), Some(adaptive));
    }

    #[test]
    fn async_stack_result_distinguishes_merged_pending_and_unknown() {
        let sha = "6dcb09b5b57875f334f61aebed695e2e4193db5e";
        assert_eq!(
            async_merge_result(&serde_json::json!({"status":"merged","details":{"sha":sha}}))
                .unwrap(),
            Some(sha.into())
        );
        assert_eq!(async_merge_result(&serde_json::json!({"status":"pending","details":{"uuid":"630b9d5e-3f2a-4f7e-8b0c-2d5f9a8c1e42"}})).unwrap(), None);
        assert!(matches!(
            async_merge_result(&serde_json::json!({"status":"enqueued"})),
            Err(LandingError::PublicationUnknown)
        ));
        assert!(matches!(
            async_merge_result(&serde_json::json!({"status":"merged","details":{"sha":"bad"}})),
            Err(LandingError::PublicationUnknown)
        ));
        assert!(matches!(
            async_merge_result(
                &serde_json::json!({"status":"failed","details":{"message":"rejected"}})
            ),
            Err(LandingError::Refused(_))
        ));
    }

    #[test]
    fn captured_async_stack_responses_drive_real_forge() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        const MARKER: &str = "PROVENANT_ASYNC_STACK_CHILD";
        const HEAD: &str = "54ada5f7a8573a53c2b69947e5c41a7e7530999a";
        if let Some(path) = std::env::var_os(MARKER) {
            let project = PathBuf::from(path);
            let mut forge = GhForge::new(&project, &project);
            assert_eq!(
                forge.merge_stack(7, HEAD, MergeMethod::Merge).unwrap(),
                "f9c664e1f37e3afb6e899701acc85805db124757"
            );
            let calls = std::fs::read_to_string(project.join("calls")).unwrap();
            assert!(rest_calls::rest_only(&calls), "{calls}");
            assert!(calls.contains(&format!("sha={HEAD}")), "{calls}");
            assert!(calls.contains("merge_method=merge"), "{calls}");
            assert!(calls.contains("merge_action=direct_merge"), "{calls}");
            assert!(
                calls.contains("merge-async/5c3f2693-53bb-4b5a-83c1-08e23438a612"),
                "{calls}"
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // Captured from the private sandbox, 2026-11-07; only async bodies are live.
        std::fs::write(
            dir.path().join("pending.json"),
            include_str!("fixtures/async-stack-pending.json"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("merged.json"),
            include_str!("fixtures/async-stack-merged.json"),
        )
        .unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(&gh, r##"#!/bin/sh
echo "$*" >> calls
case "$*" in
  *"--method PUT"*) touch merged; cat pending.json ;;
  *"merge-async/"*) cat merged.json ;;
  *"stacks/42"*) echo '{"number":42,"pull_requests":[{"number":7,"state":"open","merged_at":null},{"number":8,"state":"open","merged_at":null}]}' ;;
  *"pulls/8"*) echo '{"state":"open","head":{"sha":"54ada5f7a8573a53c2b69947e5c41a7e7530999a","ref":"upper"},"base":{"sha":"54ada5f7a8573a53c2b69947e5c41a7e7530999a","ref":"main"},"merged_at":null,"title":"upper"}' ;;
  *"pulls/7"*)
    if test -e merged; then
      echo '{"state":"closed","head":{"sha":"54ada5f7a8573a53c2b69947e5c41a7e7530999a","ref":"lower"},"base":{"sha":"54ada5f7a8573a53c2b69947e5c41a7e7530999a","ref":"main"},"merged_at":"2026-01-01T00:00:00Z","merge_commit_sha":"f9c664e1f37e3afb6e899701acc85805db124757","title":"lower"}'
    else
      echo '{"state":"open","head":{"sha":"54ada5f7a8573a53c2b69947e5c41a7e7530999a","ref":"lower"},"base":{"sha":"54ada5f7a8573a53c2b69947e5c41a7e7530999a","ref":"main"},"merged_at":null,"title":"lower","stack":{"number":42,"size":2,"position":1,"base":{"ref":"main"}}}'
    fi ;;
  *) exit 1 ;;
esac
"##).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap()
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pr::landing::adapters::tests::captured_async_stack_responses_drive_real_forge",
                "--nocapture",
            ])
            .env(MARKER, dir.path())
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    use crate::pr::landing::fakes::FakeForge;
    use serde_json::json;
    use std::process::Command;

    #[test]
    fn pull_info_reads_stack_membership_from_rest() {
        let head = "1".repeat(40);
        let mut payload = json!({
            "state":"open", "head":{"sha":head,"ref":"upper","repo":null},
            "base":{"sha":head,"ref":"lower","repo":null},
            "mergeable":true, "mergeable_state":"clean", "merged_at":null,
            "merge_commit_sha":null, "title":"fixture", "user":{"login":"author"},
            "stack":{"number":42,"size":2,"position":2,"base":{"ref":"main","sha":head}}
        });
        let info = pull_info(102, serde_json::from_value(payload.clone()).unwrap());
        assert_eq!(
            info.stack,
            Some(StackMembership {
                number: 42,
                size: 2,
                position: 2,
                base_branch: "main".into()
            })
        );
        payload.as_object_mut().unwrap().remove("stack");
        assert_eq!(
            pull_info(102, serde_json::from_value(payload).unwrap()).stack,
            None
        );
    }

    #[test]
    fn stack_response_retains_bottom_to_top_order_and_state() {
        let response = json!({"number":42,"base":{"ref":"main"},"pull_requests":[
            {"number":101,"state":"closed","merged_at":"2026-01-01T00:00:00Z"},
            {"number":102,"state":"open","merged_at":null},
            {"number":103,"state":"open","merged_at":null}
        ]});
        assert_eq!(
            stack_members(&response).unwrap(),
            vec![
                StackMember {
                    number: 101,
                    state: PullRequestState::Merged
                },
                StackMember {
                    number: 102,
                    state: PullRequestState::Open
                },
                StackMember {
                    number: 103,
                    state: PullRequestState::Open
                },
            ]
        );
        assert!(stack_members(&json!({"pull_requests":[{"number":101,"state":"open","merged_at":null},{"number":101,"state":"open","merged_at":null}]})).is_err());
    }

    mod rest_calls {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/rest_calls.rs"
        ));
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = provenant_os::fixture::git_command()
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
        repo: GitRepo<'static>,
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
        git(&work, &["config", "user.name", "Test Fixture"]);
        git(&work, &["config", "user.email", "fixture@example.invalid"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        commit_file(&work, "a", "a\n");
        git(&work, &["push", "-q", "origin", "main"]);
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let repo = GitRepo::new(&work, &state).unwrap();
        Rig {
            _dir: dir,
            work,
            bare,
            repo,
        }
    }

    #[test]
    fn mutation_failures_classify_refusals_and_timeouts() {
        for status in [405, 409, 422] {
            let error = wait::failure("MERGE_FAILED", format!("HTTP {status} refusal"));
            assert!(
                matches!(mutation_failed(error), LandingError::Refused(_)),
                "HTTP {status}"
            );
        }
        assert!(matches!(
            mutation_failed(Box::new(TimedOut)),
            LandingError::PublicationUnknown
        ));
    }

    #[test]
    fn gh_identity_and_revert_adoption_use_real_adapter() {
        use std::os::unix::fs::PermissionsExt;
        const MARKER: &str = "PROVENANT_GH_ADAPTER_CHILD";
        if let Some(project) = std::env::var_os(MARKER) {
            let project = PathBuf::from(project);
            git(&project, &["init", "-q"]);
            git(
                &project,
                &["remote", "add", "origin", "git@github.com:Owner/Repo.git"],
            );
            let mut forge = GhForge::new(&project, &project);
            let ssh = forge.repository_identity().unwrap();
            const HEAD: &str = "1111111111111111111111111111111111111111";
            assert_eq!(forge.pull_request(7).unwrap().head, HEAD);
            assert!(forge.approvals(7, HEAD).unwrap().is_empty());
            assert_eq!(forge.checks(HEAD).unwrap().len(), 1);
            assert_eq!(forge.merge(7, HEAD, MergeMethod::Merge).unwrap(), HEAD);

            git(
                &project,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "https://github.com/owner/repo.git",
                ],
            );
            assert_eq!(forge.repository_identity().unwrap(), ssh);
            std::fs::write(project.join("repo.json"), r#"{"html_url":"https://github.com/Other/Repo","full_name":"Other/Repo","owner":{"login":"Other"}}"#).unwrap();
            assert_ne!(forge.repository_identity().unwrap(), ssh);
            std::fs::write(project.join("repo.json"), r#"{"html_url":"https://github.com/Owner/Repo","full_name":"Owner/Repo","owner":{"login":"Owner"}}"#).unwrap();
            assert_eq!(
                forge.open_revert(7, "land/revert/7", "main", "ci").unwrap(),
                42
            );
            let calls = std::fs::read_to_string(project.join("calls")).unwrap();
            let landing_calls = calls
                .split("api repos/{owner}/{repo}/pulls --method POST")
                .next()
                .unwrap();
            assert!(rest_calls::rest_only(landing_calls), "{landing_calls}");
            assert!(!rest_calls::rest_only(&format!(
                "{landing_calls}api graphql -f query=x\n"
            )));
            assert!(!rest_calls::rest_only(&format!(
                "{landing_calls}pr list --head branch\n"
            )));
            assert!(calls.contains(&format!("sha={HEAD}")), "{calls}");
            assert!(calls.contains(REST_API_VERSION_HEADER), "{calls}");
            assert_eq!(
                calls
                    .lines()
                    .filter(|line| line.contains("--method POST"))
                    .count(),
                1
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(&gh, r##"#!/bin/sh
echo "$*" >> calls
case "$*" in
  *"--method PUT"*) echo '{"merged":true,"sha":"1111111111111111111111111111111111111111"}' ;;
  *"check-runs?"*) printf 'HTTP/2.0 200 OK\r\nETag: "runs"\r\n\r\n{"total_count":0,"check_runs":[]}' ;;
  *"statuses?"*) printf 'HTTP/2.0 200 OK\r\nETag: "statuses"\r\n\r\n[]' ;;
  *"git/commits/"*) echo '{"tree":{"sha":"2222222222222222222222222222222222222222"}}' ;;
  *"pulls/7/reviews"*) echo '[[]]' ;;
  *"pulls/7"*) echo '{"state":"open","head":{"sha":"1111111111111111111111111111111111111111","ref":"feature","repo":{"full_name":"Owner/Repo"}},"base":{"sha":"1111111111111111111111111111111111111111","ref":"main","repo":{"full_name":"Owner/Repo"}},"mergeable":true,"mergeable_state":"clean","merged_at":null,"merge_commit_sha":null,"title":"fixture"}' ;;
  *"--method POST"*) touch posted; echo 'HTTP 422 duplicate' >&2; exit 1 ;;
  *"--paginate"*)
    if test -e posted; then
      echo '[[{"number":41,"head":{"ref":"land/revert/7","repo":{"full_name":"evil/repo"}},"base":{"ref":"main"},"title":"Revert pull request #7","body":"Reverts pull request #7 after post-merge check failure: ci."},{"number":42,"head":{"ref":"land/revert/7","repo":{"full_name":"Owner/Repo"}},"base":{"ref":"main"},"title":"Revert pull request #7","body":"Reverts pull request #7 after post-merge check failure: ci."}]]'
    else echo '[[]]'; fi ;;
  *) cat repo.json ;;
esac
"##).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.path().join("repo.json"), r#"{"html_url":"https://github.com/Owner/Repo","full_name":"Owner/Repo","owner":{"login":"Owner"}}"#).unwrap();
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap()
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pr::landing::adapters::tests::gh_identity_and_revert_adoption_use_real_adapter",
                "--nocapture",
            ])
            .env(MARKER, dir.path())
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn batch_driver_uses_rest_forge_calls() {
        use crate::pr::landing::{
            driver,
            fakes::{FakeAlarms, FakeGate, FakeRepo},
            policy::LandPolicy,
        };
        use provenant_cli::domains::{
            LandOverride, LandSettings, PrCiMode, PrCiSettings, Publication,
        };
        use std::os::unix::fs::PermissionsExt;
        const MARKER: &str = "PROVENANT_BATCH_REST_CHILD";
        const HEAD: &str = "1111111111111111111111111111111111111111";
        if let Some(path) = std::env::var_os(MARKER) {
            let project = PathBuf::from(path);
            let mut forge = GhForge::new(&project, &project);
            let mut repo = FakeRepo {
                target: "a".repeat(40),
                valid_commit_ids: true,
                ..Default::default()
            };
            let mut gate = FakeGate {
                auto: true,
                ..Default::default()
            };
            let settings = LandSettings {
                batch_wait_seconds: 0,
                publish: Publication::RestOrdered,
                pr_ci: PrCiSettings {
                    mode: PrCiMode::None,
                    ..Default::default()
                },
                ..Default::default()
            };
            let policy = LandPolicy::resolve(
                &settings,
                &Default::default(),
                None,
                None,
                &LandOverride::default(),
            )
            .unwrap();
            let definition = super::super::receipt::tests::definition();
            let report = driver::run(
                &mut forge,
                &mut repo,
                &mut gate,
                &mut FakeAlarms::default(),
                driver::Request {
                    pins: &Default::default(),
                    numbers: &[7],
                    target: "main",
                    settings: &settings,
                    policy: &policy,
                    gate: &definition,
                    shadow: false,
                    state: &project,
                    approvals: super::super::approval::Approvals::forge(),
                },
            )
            .unwrap();
            assert_eq!(report.outcomes.get(&7), Some(&Outcome::Landed));
            let calls = std::fs::read_to_string(project.join("calls")).unwrap();
            assert!(rest_calls::rest_only(&calls), "{calls}");
            assert!(calls.contains(&format!("sha={HEAD}")), "{calls}");
            assert!(!rest_calls::rest_only(&format!(
                "{calls}api graphql -f query=x\n"
            )));
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(&gh, r##"#!/bin/sh
echo "$*" >> calls
case "$*" in
  *"--method PUT"*) echo '{"merged":true,"sha":"00000000000000000000000000000000000003e8"}' ;;
  *"pulls/7/reviews"*) echo '[[{"id":1,"user":{"login":"reviewer"},"state":"APPROVED","commit_id":"1111111111111111111111111111111111111111"}]]' ;;
  *"pulls/7"*) echo '{"state":"open","head":{"sha":"1111111111111111111111111111111111111111","ref":"feature","repo":{"full_name":"owner/repo"}},"base":{"sha":"1111111111111111111111111111111111111111","ref":"main","repo":{"full_name":"owner/repo"}},"mergeable":true,"mergeable_state":"clean","merged_at":null,"merge_commit_sha":null,"title":"fixture","user":{"login":"author"}}' ;;
  *) echo '{"html_url":"https://github.com/owner/repo","full_name":"owner/repo"}' ;;
esac
"##).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap()
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pr::landing::adapters::tests::batch_driver_uses_rest_forge_calls",
                "--nocapture",
            ])
            .env(MARKER, dir.path())
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn lost_revert_creation_response_adopts_matching_pull_request() {
        let mut lookups = 0;
        let result = reconcile_revert_creation(Err(LandingError::PublicationUnknown), || {
            lookups += 1;
            Ok(Some(42))
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(lookups, 1);
        assert!(
            reconcile_revert_creation(Err(LandingError::PublicationUnknown), || Ok(None)).is_err()
        );
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
        let sentinel = format!("{base}\t\tbranch 'other' of elsewhere\n");
        std::fs::write(rig.work.join(".git/FETCH_HEAD"), &sentinel).unwrap();
        assert_eq!(rig.repo.fetch_target_untracked("main").unwrap(), moved);
        assert_eq!(
            std::fs::read_to_string(rig.work.join(".git/FETCH_HEAD")).unwrap(),
            sentinel
        );
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
    fn commit_tree_without_identity_reports_configuration() {
        const MARKER: &str = "PROVENANT_MISSING_GIT_IDENTITY_TEST";
        if std::env::var_os(MARKER).is_some() {
            let mut rig = rig();
            git(&rig.work, &["config", "--unset", "user.name"]);
            git(&rig.work, &["config", "--unset", "user.email"]);
            git(&rig.work, &["config", "user.useConfigOnly", "true"]);
            let base = git(&rig.work, &["rev-parse", "HEAD"]);
            let tree = rig.repo.tree_of(&base).unwrap();
            let error = rig
                .repo
                .commit_tree(&tree, &[base], "candidate")
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("configure Git user.name and user.email"),
                "{error}"
            );
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pr::landing::adapters::tests::commit_tree_without_identity_reports_configuration",
            ])
            .env(MARKER, "1")
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_COUNT", "0")
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .env_remove("EMAIL")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn commit_tree_records_parents_in_order_and_tree_of_reads_it_back() {
        let mut rig = rig();
        let base = git(&rig.work, &["rev-parse", "HEAD"]);
        git(&rig.work, &["checkout", "-q", "-b", "side"]);
        let side = commit_file(&rig.work, "s", "s\n");
        let tree = rig.repo.tree_of(&side).unwrap();
        assert_eq!(
            tree,
            git(&rig.work, &["rev-parse", &format!("{side}^{{tree}}")])
        );
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
        assert!(matches!(stale, Err(LandingError::TargetMoved)), "{stale:?}");
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
            provenant_os::fixture::git_command()
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
            &mut || CheckObservation {
                checks: Ok(vec![samples.pop_front().unwrap()]),
                next_interval: None,
            },
        )
        .unwrap();
        assert_eq!(result.result, GateResult::Passed);
        assert_eq!(result.duration_seconds, Some(3));
        assert_eq!(pauses, [Duration::from_secs(3)]);
        assert_eq!(result.pool.as_deref(), Some("app"));
    }

    #[test]
    fn pending_check_uses_the_adapter_interval_before_the_next_read() {
        let settings = provenant_cli::wait::Settings {
            pr_interval_ms: 37,
            ..Default::default()
        };
        let fast = Duration::from_millis(settings.pr_interval_ms);
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
            &mut || CheckObservation {
                checks: Ok(vec![samples.pop_front().unwrap()]),
                next_interval: Some(fast),
            },
        )
        .unwrap();
        assert_eq!(result.result, GateResult::Passed);
        assert_eq!(pauses, [fast]);
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
