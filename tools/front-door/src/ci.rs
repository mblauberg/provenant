//! `ci failures`: one call for the failing side of CI.
//!
//! For a pull request or one Actions run, this reads the failed jobs' logs and
//! extracts what a chair acts on: failing nextest (or legacy `cargo test`)
//! test names, panic locations and messages, and error lines. Output stays
//! compact by default; `--json` carries the same facts structured. Reads share
//! the forge port and the CI observer's cache directory and rate-limit
//! hold-off, so a REST quota that runs out falls back to the batched GraphQL
//! read and the other way round.
use crate::pr::observer::{ObserveError, Observer};
use crate::pr::status::batch::{self, RawCheck};
use crate::pr::{forge_execute, successful};
use crate::{emit_summarised, fail, roots};
use provenant_cli::Globals;
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

/// How long one forge read may take.
const READ_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);
/// Compaction bounds; a red wave stays readable, and nothing is dropped
/// silently: the `truncated` flag records what the bounds held back.
const MAX_TESTS_PER_JOB: usize = 20;
const MAX_LINES_PER_TEST: usize = 8;
const MAX_ERROR_LINES: usize = 20;

pub(crate) fn run(command: provenant_cli::Ci, globals: &Globals) -> u8 {
    match command {
        provenant_cli::Ci::Failures { pr, target } => {
            let target = match (pr.is_empty(), target) {
                (false, None) => provenant_cli::ci::FailureTarget::Pr { numbers: pr },
                (true, Some(target)) => target,
                (false, Some(provenant_cli::ci::FailureTarget::Pr { numbers })) => {
                    let mut combined = numbers;
                    combined.extend(pr);
                    provenant_cli::ci::FailureTarget::Pr { numbers: combined }
                }
                (false, Some(provenant_cli::ci::FailureTarget::Run { .. })) => {
                    return fail(
                        "ci failures",
                        Outcome::Error,
                        "CI_FAILURES",
                        "cannot specify both --pr and a run subcommand",
                        "drop either --pr or the run subcommand and retry",
                        globals,
                    );
                }
                (true, None) => {
                    return fail(
                        "ci failures",
                        Outcome::Error,
                        "CI_FAILURES",
                        "provide pull request numbers (`pr <N>...` or `--pr <N>`) or an Actions run id (`run <ID>`)",
                        "rerun with `pr <N>...` or `run <ID>`",
                        globals,
                    );
                }
            };
            match target {
                provenant_cli::ci::FailureTarget::Pr { numbers } => failures_pr(numbers, globals),
                provenant_cli::ci::FailureTarget::Run { run } => failures_run(run, globals),
            }
        }
    }
}

/// How a job ended. Cancellation is never reported as a real failure: a job
/// cancelled by the chair, and an aggregate gate whose only bad inputs were
/// cancelled, both settle as cancelled. A gate whose inputs cannot be shown to
/// be the cause stays a real failure, marked uncertain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JobClass {
    #[default]
    Failed,
    Cancelled,
    /// Failed only because the jobs it gates were cancelled, not because any
    /// of its own inputs failed.
    CancelledDependencies,
    /// The gate failed and its inputs could not be established, so a cancelled
    /// sibling elsewhere in the run is not evidence that cancellation caused
    /// it. It stays a real failure rather than being demoted.
    Uncertain,
}
impl JobClass {
    /// Both cancelled classes are cancellation, not a defect to act on. An
    /// uncertain gate is still a failure to act on.
    fn is_cancelled(self) -> bool {
        matches!(self, Self::Cancelled | Self::CancelledDependencies)
    }
}

/// One failed job's evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JobFailures {
    pub job: String,
    /// The step that failed, when the Actions job metadata names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// How the job ended.
    #[serde(default)]
    pub class: JobClass,
    /// `possibly unrelated: PATH untouched by this PR` advisories for
    /// failures in files this pull request did not change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unrelated: Vec<String>,
    /// Failing tests in log order.
    pub tests: Vec<TestFailure>,
    /// Error lines that belong to no failing test, such as a build failure.
    pub errors: Vec<String>,
    /// True when more failing tests or error lines existed than the bounds
    /// kept.
    pub truncated: bool,
}

/// One failing test with its panic, when the log records one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TestFailure {
    pub name: String,
    /// The nextest duration stamp, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panic: Option<Panic>,
    /// Error lines printed under this test before the next failure.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<String>,
}

/// Where a test panicked and what it said.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Panic {
    pub location: String,
    pub message: String,
}

/// A failed check whose evidence is not an Actions run log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Unlogged {
    pub check: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// What `ci failures` reports: the runs it read and what they failed with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Report {
    /// The pull request number, when the target was one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    /// The Actions run id, when the target was one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// One entry per failed Actions run that carries logs.
    pub runs: Vec<RunFailures>,
    /// Failed checks with no Actions run behind them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unlogged: Vec<Unlogged>,
    /// A suggested rerun command. This is advice only; no command ever reruns
    /// jobs automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerun: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunFailures {
    pub run: u64,
    pub jobs: Vec<JobFailures>,
    /// The run's failed-job logs could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn failures_pr(numbers: Vec<u64>, globals: &Globals) -> u8 {
    const LEAF: &str = "ci failures";
    let result = (|| -> Result<Vec<Report>, Box<dyn std::error::Error>> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let state = roots::resolve(globals, roots::Workspace::Project, roots::Product::Optional)?
            .roots
            .state;
        let request = provenant_config::ports::Request::within(READ_BUDGET);
        let batch_result = batch::snapshot(state.as_path(), &project, &numbers);
        let try_graphql = !matches!(batch_result, batch::Batch::RateLimited(_));
        let entries_by_num: BTreeMap<u64, &batch::Entry> = match &batch_result {
            batch::Batch::Entries(entries) => entries.iter().map(|e| (e.number, e)).collect(),
            _ => BTreeMap::new(),
        };

        let mut reports = Vec::with_capacity(numbers.len());
        for &number in &numbers {
            let (head, repository, checks) = match entries_by_num.get(&number) {
                Some(entry) if !entry.truncated && entry.state != "UNAVAILABLE" => (
                    entry.head.clone(),
                    repository_of(&entry.url),
                    entry.checks.clone(),
                ),
                _ => resolve_pr_rest(state.as_path(), &project, number, &request, try_graphql)?,
            };
            let Some(repository) = repository else {
                return Err(
                    format!("pull request {number} names no repository to read logs from").into(),
                );
            };
            reports.push(report(
                state.as_path(),
                &project,
                &repository,
                Some(number),
                Some(head),
                &checks,
                &request,
            )?);
        }
        Ok(reports)
    })();
    emit_reports(LEAF, result, globals)
}

fn failures_run(run: u64, globals: &Globals) -> u8 {
    const LEAF: &str = "ci failures";
    let result = (|| -> Result<Report, Box<dyn std::error::Error>> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let state = roots::resolve(globals, roots::Workspace::Project, roots::Product::Optional)?
            .roots
            .state;
        let request = provenant_config::ports::Request::within(READ_BUDGET);
        let repository = batch::repository(state.as_path(), &project)?;
        let RunDigest { jobs, error } =
            run_digest(state.as_path(), &project, &repository, run, None, &request);
        Ok(Report {
            pr: None,
            run: Some(run),
            head: None,
            runs: vec![RunFailures {
                run,
                jobs: compact(jobs),
                error: error.map(|error| bounded(&error)),
            }],
            unlogged: Vec::new(),
            rerun: Some(rerun_hint(run)),
        })
    })();
    emit(LEAF, result, globals)
}

/// The pull request's head and failed checks through the REST reads, which
/// remain the path when the batch transport is unavailable. A REST rate limit
/// falls back to one GraphQL query, and only when that query may still answer.
type PrObservation = (String, Option<String>, Vec<RawCheck>);

fn resolve_pr_rest(
    state: &Path,
    project: &Path,
    number: u64,
    request: &provenant_config::ports::Request,
    try_graphql: bool,
) -> Result<PrObservation, Box<dyn std::error::Error>> {
    #[derive(Deserialize)]
    struct View {
        #[serde(rename = "headRefOid")]
        head: String,
    }
    // The head and repository come from the one observer, never a direct gh
    // read: it selects the forge port and honours the shared rate-limit hold-off.
    let resolved: Result<(String, View), String> =
        match crate::pr::observer::repository_for_project(state, project, request) {
            Ok(repository) => match Observer::open(state, project, &repository, request).and_then(
                |mut observer| {
                    observer.resource(
                        &format!("pulls/{number}"),
                        std::time::Duration::ZERO,
                        request,
                    )
                },
            ) {
                Ok(resource) => serde_json::from_value::<View>(serde_json::json!({
                    "headRefOid": resource["head"]["sha"],
                }))
                .map(|view| (repository, view))
                .map_err(|error| error.to_string()),
                Err(ObserveError::RateLimited { reset }) => {
                    Err(format!("GitHub rate limit; reset {reset}"))
                }
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        };
    let (repository, view) = match resolved {
        Ok(value) => value,
        Err(error) if try_graphql && rate_limited(&error) => {
            match batch::single(state, project, number) {
                batch::Batch::Entries(entries) if !entries.is_empty() && !entries[0].truncated => {
                    return Ok((
                        entries[0].head.clone(),
                        repository_of(&entries[0].url),
                        entries[0].checks.clone(),
                    ));
                }
                _ => return Err(format!(
                    "pull request {number} is rate limited and the batched read could not answer"
                )
                .into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    let checks = match Observer::open(state, project, &repository, request)
        .and_then(|mut observer| observer.checks(&view.head, std::time::Duration::ZERO, request))
    {
        Ok(checks) => checks
            .into_iter()
            .filter(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
            .map(|check| RawCheck {
                name: check.name,
                bucket: check.bucket,
                url: check.url,
            })
            .collect(),
        Err(ObserveError::RateLimited { reset }) if try_graphql => {
            match batch::single(state, project, number) {
                batch::Batch::Entries(entries) if !entries.is_empty() && !entries[0].truncated => {
                    return Ok((
                        entries[0].head.clone(),
                        repository_of(&entries[0].url),
                        entries[0].checks.clone(),
                    ));
                }
                _ => return Err(format!("GitHub rate limit; reset {reset}").into()),
            }
        }
        Err(error) => return Err(error.to_string().into()),
    };
    Ok((view.head, Some(repository), checks))
}

/// A rate-limited read names the limit in its message, whatever transport
/// produced it.
fn rate_limited(error: &str) -> bool {
    error.to_ascii_lowercase().contains("rate limit")
}

/// The base repository a pull request URL names, whose CI the checks ran on.
pub(crate) fn repository_of(url: &str) -> Option<String> {
    let segments: Vec<&str> = url.trim_end_matches('/').split('/').collect();
    let [.., owner, name, "pull", _] = segments.as_slice() else {
        return None;
    };
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// The shared per-job digest for a pull request head. `pr status` and
/// `wait --pr` embed this alongside `ci failures` so every command reports the
/// same failed steps, tests and errors.
pub(crate) fn digest(
    state: &Path,
    project: &Path,
    repository: &str,
    pr: u64,
    head: &str,
    checks: &[RawCheck],
    request: &provenant_config::ports::Request,
) -> Result<Report, Box<dyn std::error::Error>> {
    report(
        state,
        project,
        repository,
        Some(pr),
        Some(head.to_owned()),
        checks,
        request,
    )
}

/// Compact digest rows for an embedded report, reused by status and wait.
pub(crate) fn digest_rows(report: &Report) -> Vec<String> {
    rows(report)
}

/// Full digest text for an embedded report, reused by status and wait.
pub(crate) fn digest_human(report: &Report) -> String {
    human(report)
}

/// Failed Actions runs behind the checks, plus checks with no Actions run.
fn report(
    state: &Path,
    project: &Path,
    repository: &str,
    pr: Option<u64>,
    head: Option<String>,
    checks: &[RawCheck],
    request: &provenant_config::ports::Request,
) -> Result<Report, Box<dyn std::error::Error>> {
    let mut runs: Vec<RunFailures> = Vec::new();
    let mut unlogged = Vec::new();
    // The changed-file list is read once per pull request, not per run: the
    // "possibly unrelated" advisory compares every failure against the same
    // tree delta. An unreadable list stays absent rather than becoming an empty
    // list that would label every failure unrelated.
    let changed = pr.and_then(|number| changed_files(state, project, repository, number, request));
    for check in checks
        .iter()
        .filter(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
    {
        let Some(run) = check.url.as_deref().and_then(actions_run) else {
            unlogged.push(Unlogged {
                check: check.name.clone(),
                url: check.url.clone(),
            });
            continue;
        };
        if runs.iter().any(|known: &RunFailures| known.run == run) {
            continue;
        }
        let RunDigest { jobs, error } =
            run_digest(state, project, repository, run, changed.as_deref(), request);
        runs.push(RunFailures {
            run,
            jobs: compact(jobs),
            error: error.map(|error| bounded(&error)),
        });
    }
    let rerun = runs.first().map(|run| rerun_hint(run.run));
    Ok(Report {
        pr,
        run: None,
        head,
        runs,
        unlogged,
        rerun,
    })
}

/// The Actions run id a check run's details URL names.
fn actions_run(url: &str) -> Option<u64> {
    let at = url.find("/actions/runs/")?;
    let rest = &url[at + "/actions/runs/".len()..];
    let digits = rest.split(['/', '#', '?']).next()?;
    (digits.len() < 20 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// A run's failed jobs and an error describing any unreadable metadata or log.
struct RunDigest {
    jobs: Vec<JobFailures>,
    error: Option<String>,
}

/// One job from `actions/runs/{id}/jobs`, the metadata the digest needs to
/// classify the job and name its failed step.
#[derive(Deserialize)]
struct ApiJob {
    id: u64,
    name: String,
    #[serde(default)]
    status: String,
    conclusion: Option<String>,
    #[serde(default)]
    steps: Vec<ApiStep>,
    /// A forge that publishes dependencies on the jobs endpoint lets us read
    /// the gate's inputs without fetching the workflow. GitHub omits this
    /// field, which is what sends the reader to the workflow file.
    #[serde(default)]
    needs: Option<Needs>,
}

#[derive(Deserialize)]
struct ApiStep {
    name: String,
    conclusion: Option<String>,
}

/// GitHub spells a single dependency as a scalar and several as a sequence,
/// both in the jobs metadata and in a workflow's `needs:` key.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum Needs {
    One(String),
    Many(Vec<String>),
}

impl Needs {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(name) => vec![name],
            Self::Many(names) => names,
        }
    }
}

/// Whether a job conclusion is a bad input to a gate. A skipped job is bad
/// input too: it means the gate lost a member.
fn bad_input(conclusion: Option<&str>) -> bool {
    matches!(
        conclusion,
        Some("failure")
            | Some("timed_out")
            | Some("action_required")
            | Some("startup_failure")
            | Some("cancelled")
            | Some("skipped")
    )
}

/// The jobs the aggregate gate actually waits for. `complete` is false when a
/// dependency could not be matched to an ended job, so conclusions from the
/// rest of the run cannot establish why the gate failed.
#[derive(Debug, Default)]
struct GateInputs {
    ids: Vec<u64>,
    complete: bool,
}

impl GateInputs {
    fn unknown() -> Self {
        Self::default()
    }
}

/// Classify every ended job in a run. `gh run view --log-failed` waits for the
/// whole run; the jobs endpoint names each job's own conclusion as soon as that
/// job ends. An aggregate gate is demoted only when its own dependencies show
/// cancellations and no other cause.
fn classify_jobs(jobs: &[ApiJob], inputs: &GateInputs) -> BTreeMap<u64, JobClass> {
    let mut classes = BTreeMap::new();
    for job in jobs {
        if job.status != "completed" {
            continue;
        }
        let class = if job.name == AGGREGATE_GATE {
            gate_class(job.conclusion.as_deref(), jobs, inputs)
        } else {
            match job.conclusion.as_deref() {
                Some("cancelled") | Some("skipped") => Some(JobClass::Cancelled),
                Some(conclusion) if bad_input(Some(conclusion)) => Some(JobClass::Failed),
                // A succeeded (or neutral) job is not part of the failing digest.
                _ => None,
            }
        };
        let Some(class) = class else { continue };
        classes.insert(job.id, class);
    }
    classes
}

/// The repository's required aggregate check. Its own conclusion is a failure
/// that says nothing about its cause; the jobs it waits for do.
const AGGREGATE_GATE: &str = "ci-status";

/// Classify the aggregate gate from its dependencies alone. A direct
/// cancellation needs no inference; every other bad conclusion needs complete
/// dependency evidence before the gate may be called a cancellation.
fn gate_class(conclusion: Option<&str>, jobs: &[ApiJob], inputs: &GateInputs) -> Option<JobClass> {
    match conclusion {
        Some("cancelled") => Some(JobClass::Cancelled),
        Some(conclusion) if bad_input(Some(conclusion)) => {
            if !inputs.complete {
                Some(JobClass::Uncertain)
            } else if gate_only_cancelled(jobs, inputs) {
                Some(JobClass::CancelledDependencies)
            } else {
                Some(JobClass::Failed)
            }
        }
        _ => None,
    }
}

/// Whether every bad conclusion among the gate's own inputs was a cancellation
/// or skip, with at least one such input. Jobs elsewhere in the run are not
/// evidence either way and are never consulted here.
fn gate_only_cancelled(jobs: &[ApiJob], inputs: &GateInputs) -> bool {
    let mut saw_bad = false;
    for job in jobs {
        if !inputs.ids.contains(&job.id) {
            continue;
        }
        let Some(conclusion) = job.conclusion.as_deref() else {
            continue;
        };
        if !bad_input(Some(conclusion)) {
            continue;
        }
        saw_bad = true;
        if !matches!(conclusion, "cancelled" | "skipped") {
            return false;
        }
    }
    saw_bad
}

/// Dependency names published on the jobs endpoint, when the forge provides
/// them. A dependency that names no ended job leaves the inputs incomplete.
fn inputs_from_names(needs: &[String], jobs: &[ApiJob]) -> GateInputs {
    let mut ids = Vec::new();
    let mut complete = true;
    for need in needs {
        let matches: Vec<u64> = jobs
            .iter()
            .filter(|job| job.name == *need || job.id.to_string() == *need)
            .map(|job| job.id)
            .collect();
        if matches.is_empty() {
            complete = false;
        }
        ids.extend(matches);
    }
    GateInputs { ids, complete }
}

/// Read the gate's `needs` from its own job metadata, when the jobs endpoint
/// publishes them.
fn gate_inputs_from_jobs(jobs: &[ApiJob]) -> Option<GateInputs> {
    let gate = jobs.iter().find(|job| job.name == AGGREGATE_GATE)?;
    let names = gate.needs.clone()?.into_vec();
    (!names.is_empty()).then(|| inputs_from_names(&names, jobs))
}

/// A dependency as declared in the workflow: its job key and, when set, the
/// display name the jobs API reports. Matrix names are templates the runner
/// expands with concrete values.
struct WorkflowNeed {
    key: String,
    name: Option<String>,
    matrix: Option<WorkflowMatrix>,
}

impl WorkflowNeed {
    /// The display names this declaration is expected to produce. A matrix
    /// name expands to every declared combination, so a member that never
    /// produced a job is visible as missing rather than silently tolerated.
    fn patterns(&self) -> Vec<String> {
        let pattern = self.name.as_deref().unwrap_or(&self.key);
        self.matrix
            .as_ref()
            .and_then(|matrix| expand_matrix_name(pattern, matrix))
            .unwrap_or_else(|| vec![pattern.to_owned()])
    }
}

/// The part of a workflow file the digest reads: job keys, display names,
/// declared dependencies and matrix values. Every other workflow key is
/// ignored.
#[derive(Deserialize)]
struct Workflow {
    jobs: BTreeMap<String, WorkflowJob>,
}

#[derive(Deserialize)]
struct WorkflowJob {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    needs: Option<Needs>,
    #[serde(default)]
    strategy: Option<WorkflowStrategy>,
}

#[derive(Clone, Deserialize)]
struct WorkflowStrategy {
    #[serde(default)]
    matrix: Option<WorkflowMatrix>,
}

/// Matrix dimensions are lists of scalar values. Runner-only keys such as
/// `include` hold mappings and are never used to expand a display name.
#[derive(Clone, Deserialize)]
struct WorkflowMatrix {
    #[serde(flatten)]
    dimensions: BTreeMap<String, Value>,
}

fn matrix_values(value: &Value) -> Option<Vec<String>> {
    let entries = value.as_array()?;
    let values: Vec<String> = entries
        .iter()
        .map(|value| match value {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (!values.is_empty() && values.len() <= 256).then_some(values)
}

/// Expand each `${{ matrix.<dimension> }}` in a display name into every
/// declared value. An unreadable or absent dimension yields no evidence: the
/// caller then falls back to matching the literal template.
fn expand_matrix_name(pattern: &str, matrix: &WorkflowMatrix) -> Option<Vec<String>> {
    let mut placeholders = Vec::new();
    let mut rest = pattern;
    while let Some(start) = rest.find("${{") {
        let tail = &rest[start + 3..];
        let close = tail.find("}}")?;
        let dimension = tail[..close].trim().strip_prefix("matrix.")?;
        if dimension.is_empty() {
            return None;
        }
        let end = start + 3 + close + 2;
        placeholders.push((start, end, dimension.to_owned()));
        rest = &rest[end..];
    }
    if placeholders.is_empty() {
        return None;
    }
    let mut names = vec![pattern.to_owned()];
    // Replace from the end so the byte offsets of earlier placeholders stay
    // valid as values of different lengths are substituted.
    for (start, end, dimension) in placeholders.into_iter().rev() {
        let values = matrix.dimensions.get(&dimension).and_then(matrix_values)?;
        let mut expanded = Vec::with_capacity(names.len() * values.len());
        for name in &names {
            for value in &values {
                let mut candidate = name.clone();
                candidate.replace_range(start..end, value);
                expanded.push(candidate);
            }
        }
        if expanded.len() > 256 {
            return None;
        }
        names = expanded;
    }
    Some(names)
}

/// Bound the workflow read the same way the front door bounds other YAML.
fn yaml_options() -> serde_saphyr::Options {
    let mut budget = serde_saphyr::Budget::default();
    budget.max_reader_input_bytes = Some(2 * 1024 * 1024);
    budget.max_depth = 32;
    budget.max_nodes = 100_000;
    let mut options = serde_saphyr::Options::default();
    options.budget = Some(budget);
    options
}

/// Read the aggregate gate's dependencies from a workflow file. A workflow
/// without the gate, or without a `needs:` declaration on it, yields no
/// evidence rather than an empty dependency list that could be mistaken for
/// one.
fn parse_workflow_needs(text: &str) -> Result<Option<Vec<WorkflowNeed>>, String> {
    let workflow: Workflow = serde_saphyr::from_str_with_options(text, yaml_options())
        .map_err(|error| error.to_string())?;
    let Some(gate) = workflow.jobs.get(AGGREGATE_GATE) else {
        return Ok(None);
    };
    let Some(needs) = gate.needs.clone() else {
        return Ok(None);
    };
    Ok(Some(
        needs
            .into_vec()
            .into_iter()
            .map(|key| {
                let declared = workflow.jobs.get(&key);
                WorkflowNeed {
                    name: declared.and_then(|job| job.name.clone()),
                    matrix: declared
                        .and_then(|job| job.strategy.clone())
                        .and_then(|strategy| strategy.matrix),
                    key,
                }
            })
            .collect(),
    ))
}

/// Map workflow dependencies to the ended jobs they produced. Every expected
/// display name must resolve to a job: a missing matrix member leaves the
/// inputs incomplete, so a cancelled sibling cannot explain the gate's failure.
fn inputs_from_workflow(needs: &[WorkflowNeed], jobs: &[ApiJob]) -> GateInputs {
    let mut ids = Vec::new();
    let mut complete = true;
    for need in needs {
        for pattern in need.patterns() {
            let matches: Vec<u64> = jobs
                .iter()
                .filter(|job| name_matches(&pattern, &job.name))
                .map(|job| job.id)
                .collect();
            if matches.is_empty() {
                complete = false;
            }
            ids.extend(matches);
        }
    }
    GateInputs { ids, complete }
}

/// Match a job display name against a workflow name, expanding each
/// `${{ ... }}` placeholder to one or more characters of the actual name.
fn name_matches(pattern: &str, name: &str) -> bool {
    let Some(first) = pattern.find("${{") else {
        return pattern == name;
    };
    let (prefix, mut rest_pattern) = pattern.split_at(first);
    if !name.starts_with(prefix) {
        return false;
    }
    let mut rest_name = &name[prefix.len()..];
    loop {
        let Some(close) = rest_pattern.find("}}") else {
            return false;
        };
        rest_pattern = &rest_pattern[close + 2..];
        let Some(open) = rest_pattern.find("${{") else {
            return rest_name.len() > rest_pattern.len() && rest_name.ends_with(rest_pattern);
        };
        let (literal, after) = rest_pattern.split_at(open);
        let Some(at) = rest_name.find(literal) else {
            return false;
        };
        if at == 0 {
            return false;
        }
        rest_name = &rest_name[at + literal.len()..];
        rest_pattern = after;
    }
}

/// Read the gate's dependencies from the workflow file at the run's head SHA.
/// Any unreadable metadata leaves the inputs unknown: the digest reports an
/// uncertain gate rather than guessing from other jobs in the run.
fn workflow_gate_inputs(
    state: &Path,
    project: &Path,
    repository: &str,
    run: u64,
    jobs: &[ApiJob],
    request: &provenant_config::ports::Request,
) -> GateInputs {
    let Ok(text) = workflow_at_head(state, project, repository, run, request) else {
        return GateInputs::unknown();
    };
    let Ok(Some(needs)) = parse_workflow_needs(&text) else {
        return GateInputs::unknown();
    };
    inputs_from_workflow(&needs, jobs)
}

/// Fetch the workflow file that produced a run, pinned to that run's head SHA.
fn workflow_at_head(
    state: &Path,
    project: &Path,
    repository: &str,
    run: u64,
    request: &provenant_config::ports::Request,
) -> Result<String, String> {
    let endpoint = format!("repos/{repository}/actions/runs/{run}");
    let bytes = successful(
        forge_execute(
            state,
            project,
            &["gh", "api", &endpoint],
            request,
            false,
            None,
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let metadata: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let head = metadata
        .get("head_sha")
        .and_then(Value::as_str)
        .ok_or("run metadata named no head SHA")?;
    let workflow_id = metadata
        .get("workflow_id")
        .and_then(Value::as_u64)
        .ok_or("run metadata named no workflow id")?;

    let endpoint = format!("repos/{repository}/actions/workflows/{workflow_id}");
    let bytes = successful(
        forge_execute(
            state,
            project,
            &["gh", "api", &endpoint],
            request,
            false,
            None,
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let metadata: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let path = metadata
        .get("path")
        .and_then(Value::as_str)
        .ok_or("workflow metadata named no path")?;

    let endpoint = format!("repos/{repository}/contents/{path}?ref={head}");
    let bytes = successful(
        forge_execute(
            state,
            project,
            &[
                "gh",
                "api",
                "-H",
                "Accept: application/vnd.github.raw+json",
                &endpoint,
            ],
            request,
            false,
            None,
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Build the per-job digest for one run. Each ended job's log is fetched on its
/// own through the `jobs/{id}/logs` endpoint, so a job's failure is available
/// as soon as that job ends rather than after the slowest job in the run.
fn run_digest(
    state: &Path,
    project: &Path,
    repository: &str,
    run: u64,
    changed: Option<&[String]>,
    request: &provenant_config::ports::Request,
) -> RunDigest {
    let jobs = match run_jobs(state, project, repository, run, request) {
        Ok(jobs) => jobs,
        Err(error) => {
            return RunDigest {
                jobs: Vec::new(),
                error: Some(error),
            };
        }
    };
    let inputs = gate_inputs_from_jobs(&jobs)
        .unwrap_or_else(|| workflow_gate_inputs(state, project, repository, run, &jobs, request));
    let classes = classify_jobs(&jobs, &inputs);
    let mut errors = Vec::new();
    let mut failures = Vec::new();
    for job in &jobs {
        let Some(class) = classes.get(&job.id).copied() else {
            continue;
        };
        let step = job
            .steps
            .iter()
            .find(|step| {
                matches!(
                    step.conclusion.as_deref(),
                    Some("failure") | Some("timed_out")
                )
            })
            .map(|step| step.name.clone());
        // An uncertain gate is still a failure to act on, so keep its log
        // evidence even though its cause could not be established.
        let mut parsed = if !class.is_cancelled() {
            match job_log(state, project, repository, job.id, request) {
                Ok(text) => parse_failed_log(&text),
                Err(error) => {
                    // Keep the gh message: an unreadable log is not evidence
                    // that the job had no failures.
                    errors.push(format!("job {}: unreadable log: {error}", job.name));
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        if parsed.is_empty() {
            parsed.push(JobFailures {
                job: job.name.clone(),
                step: step.clone(),
                class,
                unrelated: Vec::new(),
                tests: Vec::new(),
                errors: Vec::new(),
                truncated: false,
            });
        }
        for parsed_job in &mut parsed {
            parsed_job.job = job.name.clone();
            parsed_job.step = step.clone();
            parsed_job.class = class;
            if let Some(changed) = changed {
                parsed_job.unrelated = unrelated_paths(parsed_job, changed);
            }
        }
        failures.extend(parsed);
    }
    RunDigest {
        jobs: failures,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

/// Read every job of one Actions run, following pagination. A slurped response
/// is a list of pages; a single response is one page.
fn run_jobs(
    state: &Path,
    project: &Path,
    repository: &str,
    run: u64,
    request: &provenant_config::ports::Request,
) -> Result<Vec<ApiJob>, String> {
    let endpoint = format!("repos/{repository}/actions/runs/{run}/jobs");
    let bytes = successful(
        forge_execute(
            state,
            project,
            &["gh", "api", &endpoint, "--paginate", "--slurp"],
            request,
            false,
            None,
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    parse_jobs(&bytes)
}

/// Parse an Actions jobs response, following pagination. A slurped response
/// is a list of pages; a single response is one page.
fn parse_jobs(bytes: &[u8]) -> Result<Vec<ApiJob>, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let pages: Vec<&Value> = match &value {
        Value::Array(items) if items.iter().any(|item| item.get("jobs").is_some()) => {
            items.iter().collect()
        }
        _ => vec![&value],
    };
    let mut jobs = Vec::new();
    for page in pages {
        let entries = page
            .get("jobs")
            .and_then(Value::as_array)
            .or_else(|| page.as_array())
            .ok_or("Actions jobs response named no jobs")?;
        for entry in entries {
            jobs.push(serde_json::from_value(entry.clone()).map_err(|error| error.to_string())?);
        }
    }
    Ok(jobs)
}

/// Fetch one job's log. `gh api` follows the `jobs/{id}/logs` 302 redirect to
/// the signed log archive and returns the text; the redirect strips the
/// forge token at the archive host. Test logs carry terminal escape sequences,
/// which `gh api` refuses to print unless asked explicitly. The log of an
/// ended job never changes, so it is cached per job id for the short
/// observation interval.
fn job_log(
    state: &Path,
    project: &Path,
    repository: &str,
    job_id: u64,
    request: &provenant_config::ports::Request,
) -> Result<String, String> {
    let dir = crate::pr::observer::shared_dir(state, repository)
        .map_err(|error| format!("cannot open shared CI cache: {error}"))?;
    let name = format!("job-log-{job_id}.json");
    let endpoint = format!("repos/{repository}/actions/jobs/{job_id}/logs");
    let text = cached_text(state, project, &dir, &name, || {
        let bytes = successful(
            forge_execute(
                state,
                project,
                &["gh", "api", "--allow-escape-sequences", &endpoint],
                request,
                false,
                None,
            )
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    })?;
    // Strip on read as well as on fetch: a cache entry written before this
    // read path could still hold raw escape sequences.
    Ok(strip_terminal_escapes(&text))
}

/// Remove terminal control sequences from an Actions log before it is parsed.
/// Colour and cursor control graphics are not part of a failure's text, and
/// leaving them in breaks a test-name or error-line match.
fn strip_terminal_escapes(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut plain = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            let Some(character) = text[index..].chars().next() else {
                break;
            };
            plain.push(character);
            index += character.len_utf8();
            continue;
        }
        index += 1;
        let Some(kind) = bytes.get(index).copied() else {
            break;
        };
        match kind {
            // CSI: parameters and intermediates, then a final byte 0x40..0x7e.
            b'[' => {
                index += 1;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            // OSC: ends at BEL or the string terminator ESC backslash.
            b']' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            // Two-byte escape sequences such as ESC 7 or ESC ( B.
            0x40..=0x5f => index += 1,
            // An unknown escape: drop the ESC and keep reading the text.
            _ => {}
        }
    }
    plain
}

/// Share the bounded cache used by per-job logs. A completed job's log and a
/// stale observation are both harmless to reread after the freshness window.
fn cached_text(
    state: &Path,
    project: &Path,
    dir: &provenant_os::fs::Directory,
    name: &str,
    fetch: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    let ttl =
        crate::pr::observer::fast_interval(state, project).unwrap_or(crate::pr::observer::POLL);
    let now = clock();
    #[derive(Serialize, Deserialize)]
    struct Cached {
        fetched_at: i64,
        text: String,
    }
    if let Ok(bytes) = dir.read_bounded(std::ffi::OsStr::new(name), 4 * 1024 * 1024)
        && let Ok(cached) = serde_json::from_slice::<Cached>(&bytes)
    {
        let age = now.as_millisecond().saturating_sub(cached.fetched_at);
        if (0..i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX)).contains(&age) {
            return Ok(cached.text);
        }
    }
    let text = fetch()?;
    if let Ok(bytes) = serde_json::to_vec(&Cached {
        fetched_at: now.as_millisecond(),
        text: text.clone(),
    }) {
        let _ = dir.atomic_write(
            std::ffi::OsStr::new(name),
            ".job-log-",
            &bytes,
            provenant_os::fs::PublishMode::Replace,
        );
    }
    Ok(text)
}

/// The files a pull request changes. A failed read returns `None`, which
/// suppresses the "possibly unrelated" advisory instead of treating an
/// unreadable delta as an empty one.
fn changed_files(
    state: &Path,
    project: &Path,
    repository: &str,
    number: u64,
    request: &provenant_config::ports::Request,
) -> Option<Vec<String>> {
    let endpoint = format!("repos/{repository}/pulls/{number}/files");
    let bytes = successful(
        forge_execute(
            state,
            project,
            &["gh", "api", &endpoint, "--paginate", "--slurp"],
            request,
            false,
            None,
        )
        .ok()?,
    )
    .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let pages: Vec<&Value> = match &value {
        Value::Array(items) if items.iter().all(Value::is_array) => items.iter().collect(),
        _ => vec![&value],
    };
    let mut paths = Vec::new();
    for page in pages {
        let entries = page.as_array()?;
        for entry in entries {
            if let Some(path) = entry.get("filename").and_then(Value::as_str) {
                paths.push(path.to_owned());
            }
        }
    }
    Some(paths)
}

/// The unit of ownership a path belongs to: a crate directory, or the root
/// crate's `src`. Two paths in the same unit are touched by the same change.
fn ownership_unit(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized
        .strip_prefix("tools/front-door/")
        .unwrap_or(&normalized);
    let parts: Vec<&str> = normalized.split('/').collect();
    match parts.as_slice() {
        ["crates", name, ..] => format!("crates/{name}"),
        ["src", ..] => "src".to_owned(),
        _ => normalized.to_owned(),
    }
}

/// The advisory for a path a pull request did not touch. The exact wording is
/// part of the digest so it reads the same in text and in `--json`.
fn unrelated_label(path: &str) -> String {
    format!("possibly unrelated: {path} untouched by this PR")
}

/// Compare a job's failure locations and error paths against the pull request's
/// changed files. A failure in an untouched crate or file is labelled; one in
/// a touched file is not.
fn unrelated_paths(job: &JobFailures, changed: &[String]) -> Vec<String> {
    let changed_units: Vec<String> = changed.iter().map(|path| ownership_unit(path)).collect();
    let mut paths: Vec<String> = Vec::new();
    for test in &job.tests {
        if let Some(location) = &test.panic {
            paths.push(failure_path(&location.location).to_owned());
        }
    }
    for error in &job.errors {
        if let Some(path) = error_path(error) {
            paths.push(path.to_owned());
        }
    }
    let mut labels = Vec::new();
    for path in paths {
        if path.is_empty()
            || changed_units
                .iter()
                .any(|unit| *unit == ownership_unit(&path))
        {
            continue;
        }
        let label = unrelated_label(&path);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

/// The file a panic location names, without its `:line:column` suffix.
fn failure_path(location: &str) -> &str {
    let location = location
        .rsplit_once(" panicked at ")
        .map_or(location, |(_, rest)| rest);
    let trimmed = location.trim_end_matches(':');
    match trimmed.rfind(':') {
        Some(at) if trimmed[at + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            let line = &trimmed[..at];
            match line.rfind(':') {
                Some(col) if line[col + 1..].bytes().all(|b| b.is_ascii_digit()) => &line[..col],
                _ => line,
            }
        }
        _ => trimmed,
    }
}

/// A compiler error's `--> path:line:column` location.
fn error_path(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("--> ")?.trim();
    let path = rest
        .split_once(':')
        .map_or(rest, |(path, _)| path)
        .trim_end_matches(':');
    (!path.is_empty()).then_some(path)
}

/// A rerun is advice, never an action: no code here executes it.
fn rerun_hint(run: u64) -> String {
    format!("gh run rerun {run} --failed")
}

#[allow(
    clippy::disallowed_methods,
    reason = "the failures cache TTL boundary owns the wall clock"
)]
fn clock() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

/// Apply the compaction bounds, recording what the bounds held back.
fn compact(jobs: Vec<JobFailures>) -> Vec<JobFailures> {
    let mut result = Vec::with_capacity(jobs.len());
    for mut job in jobs {
        if job.tests.len() > MAX_TESTS_PER_JOB || job.errors.len() > MAX_ERROR_LINES {
            job.truncated = true;
        }
        job.tests.truncate(MAX_TESTS_PER_JOB);
        job.errors.truncate(MAX_ERROR_LINES);
        for test in &mut job.tests {
            test.lines.truncate(MAX_LINES_PER_TEST);
            test.name = bounded(&test.name);
            if let Some(panic) = &mut test.panic {
                panic.message = bounded(&panic.message);
                panic.location = bounded(&panic.location);
            }
        }
        result.push(job);
    }
    result
}

fn bounded(text: &str) -> String {
    provenant_cli::bounded_text(text, 512)
}

fn emit(leaf: &str, result: Result<Report, Box<dyn std::error::Error>>, globals: &Globals) -> u8 {
    match result {
        Ok(report) => {
            let tests = report
                .runs
                .iter()
                .map(|run| run.jobs.iter().map(|job| job.tests.len()).sum::<usize>())
                .sum::<usize>();
            let failing_jobs = report
                .runs
                .iter()
                .map(|run| real_jobs(&run.jobs))
                .sum::<usize>();
            let text = human(&report);
            let outcome = if report.runs.iter().any(|run| run.error.is_some())
                && tests == 0
                && report.unlogged.is_empty()
            {
                Outcome::Unavailable
            } else {
                Outcome::Ok
            };
            let subject = match (report.pr, report.run) {
                (Some(pr), _) => format!("pr {pr}"),
                (None, Some(run)) => format!("run {run}"),
                (None, None) => String::new(),
            };
            let headline = format!(
                "{}{failing_jobs} failed job(s), {tests} failing test(s)",
                if subject.is_empty() {
                    String::new()
                } else {
                    format!("{subject}: ")
                }
            );
            // The rerun is advice shown to the chair; nothing executes it.
            let next = report.rerun.clone();
            let summary = provenant_cli::Summary::new(headline, rows(&report), next);
            emit_summarised(
                &CommandResult::new(leaf, outcome, report),
                globals,
                &text,
                true,
                Some(&summary),
            )
        }
        Err(error) => fail(
            leaf,
            Outcome::Error,
            "CI_FAILURES",
            &error.to_string(),
            "check the reported forge read error and credentials, then rerun `provenant ci failures`",
            globals,
        ),
    }
}

fn emit_reports(
    leaf: &str,
    result: Result<Vec<Report>, Box<dyn std::error::Error>>,
    globals: &Globals,
) -> u8 {
    match result {
        Ok(reports) => {
            let mut text = String::new();
            let mut all_rows = Vec::new();
            let mut failing_jobs = 0;
            let mut tests = 0;
            for report in &reports {
                text.push_str(&human(report));
                all_rows.extend(rows(report));
                for run in &report.runs {
                    failing_jobs += real_jobs(&run.jobs);
                    tests += run.jobs.iter().map(|job| job.tests.len()).sum::<usize>();
                }
            }
            let outcome = if reports.iter().any(|r| {
                r.runs.iter().any(|run| run.error.is_some())
                    && r.runs.iter().all(|run| run.jobs.is_empty())
                    && r.unlogged.is_empty()
            }) {
                Outcome::Unavailable
            } else {
                Outcome::Ok
            };
            let subject = if reports.len() == 1 {
                reports[0]
                    .pr
                    .map(|pr| format!("pr {pr}: "))
                    .unwrap_or_default()
            } else {
                format!("{} PRs: ", reports.len())
            };
            let headline =
                format!("{subject}{failing_jobs} failed job(s), {tests} failing test(s)");
            let next = reports.iter().find_map(|report| report.rerun.clone());
            let summary = provenant_cli::Summary::new(headline, all_rows, next);
            emit_summarised(
                &CommandResult::new(leaf, outcome, reports),
                globals,
                &text,
                true,
                Some(&summary),
            )
        }
        Err(error) => fail(
            leaf,
            Outcome::Error,
            "CI_FAILURES",
            &error.to_string(),
            "check the reported forge read error and credentials, then rerun `provenant ci failures`",
            globals,
        ),
    }
}

/// Only a real failure counts as a failed job in the headline; cancelled jobs
/// and gates cancelled through their inputs are cancellation, not defects.
fn real_jobs(jobs: &[JobFailures]) -> usize {
    jobs.iter().filter(|job| !job.class.is_cancelled()).count()
}

/// The class label a chair reads; a gate cancelled through its inputs says why.
fn class_label(class: JobClass) -> &'static str {
    match class {
        JobClass::Failed => "failed",
        JobClass::Cancelled => "cancelled",
        JobClass::CancelledDependencies => "cancelled (dependencies only)",
        JobClass::Uncertain => "failed (gate inputs unknown)",
    }
}

fn rows(report: &Report) -> Vec<String> {
    let mut rows = Vec::new();
    for run in &report.runs {
        if let Some(error) = &run.error {
            rows.push(format!("run {}: unreadable: {error}", run.run));
        }
        for job in &run.jobs {
            let prefix = match &job.step {
                Some(step) => format!("run {} {} [{}]", run.run, job.job, step),
                None => format!("run {} {}", run.run, job.job),
            };
            if job.tests.is_empty() && job.errors.is_empty() {
                rows.push(format!("{prefix}: {}", class_label(job.class)));
            }
            for test in &job.tests {
                let mut row = format!("{prefix} {}: {}", class_label(job.class), test.name);
                if let Some(duration) = &test.duration {
                    row.push_str(&format!(" ({duration})"));
                }
                if let Some(panic) = &test.panic {
                    row.push_str(&format!(
                        " panicked at {}: {}",
                        panic.location, panic.message
                    ));
                }
                rows.push(row);
            }
            // A lint or compiler error belongs to no test, so keep its job,
            // failed step and classification in the default compact output.
            for error in &job.errors {
                rows.push(format!("{prefix}: {}: {error}", class_label(job.class)));
            }
            for unrelated in &job.unrelated {
                rows.push(format!("{prefix}: {unrelated}"));
            }
        }
    }
    for unlogged in &report.unlogged {
        rows.push(format!("check {}: no Actions run logs", unlogged.check));
    }
    if let Some(rerun) = &report.rerun {
        rows.push(format!("suggested: {rerun} (not run automatically)"));
    }
    rows
}

/// The compact default text: one row per failing test, then error lines.
fn human(report: &Report) -> String {
    let mut text = String::new();
    for run in &report.runs {
        if let Some(error) = &run.error {
            text.push_str(&format!("run {}: unreadable: {error}\n", run.run));
            // A failed log read still leaves failed jobs in the metadata. Show
            // them with their step and class instead of reporting zero.
            if run.jobs.is_empty() {
                continue;
            }
        }
        for job in &run.jobs {
            let step = job
                .step
                .as_deref()
                .map(|step| format!(" step {step}"))
                .unwrap_or_default();
            text.push_str(&format!(
                "run {} job {} [{}]{}: {} failing test(s)\n",
                run.run,
                job.job,
                class_label(job.class),
                step,
                job.tests.len()
            ));
            for test in &job.tests {
                text.push_str(&format!(
                    "  FAIL {}{}\n",
                    test.name,
                    test.duration
                        .as_deref()
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default()
                ));
                if let Some(panic) = &test.panic {
                    text.push_str(&format!(
                        "    panicked at {}: {}\n",
                        panic.location, panic.message
                    ));
                }
                for line in &test.lines {
                    text.push_str(&format!("    {line}\n"));
                }
            }
            for error in &job.errors {
                text.push_str(&format!("  {error}\n"));
            }
            for unrelated in &job.unrelated {
                text.push_str(&format!("  {unrelated}\n"));
            }
            if job.truncated {
                text.push_str(&format!("  job {}: more failures held back\n", job.job));
            }
        }
    }
    for unlogged in &report.unlogged {
        text.push_str(&format!(
            "check {}: no Actions run logs{}\n",
            unlogged.check,
            unlogged
                .url
                .as_deref()
                .map(|url| format!(" ({url})"))
                .unwrap_or_default()
        ));
    }
    if let Some(rerun) = &report.rerun {
        text.push_str(&format!("suggested: {rerun} (not run automatically)\n"));
    }
    text
}

/// Parse one failed-job log into per-job failures. The log may carry GitHub
/// Actions `job<TAB>step<TAB>line` prefixes and timestamps, which are stripped
/// before the content is read.
pub(crate) fn parse_failed_log(text: &str) -> Vec<JobFailures> {
    let mut jobs: Vec<JobFailures> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    // The failing test a following panic block or error line belongs to.
    let mut active: Option<(usize, usize)> = None;
    for raw in text.lines() {
        let (job_name, line) = split_job(raw);
        let line = strip_prefixes(line);
        let job = {
            let next = jobs.len();
            *index.entry(job_name.to_owned()).or_insert(next)
        };
        if jobs.len() <= job {
            jobs.push(JobFailures {
                job: job_label(job_name),
                step: None,
                class: JobClass::Failed,
                unrelated: Vec::new(),
                tests: Vec::new(),
                errors: Vec::new(),
                truncated: false,
            });
        }
        // A panic block never spans jobs.
        if matches!(active, Some((owner, _)) if owner != job) {
            active = None;
        }
        if let Some((name, duration)) = fail_line(line).or_else(|| {
            let cargo = cargo_fail(line)?;
            if active.is_some_and(|(owner, index)| {
                owner == job && jobs[owner].tests[index].name.ends_with(&cargo.0)
            }) {
                None
            } else {
                Some(cargo)
            }
        }) {
            jobs[job].tests.push(TestFailure {
                name,
                duration,
                panic: None,
                lines: Vec::new(),
            });
            active = Some((job, jobs[job].tests.len() - 1));
            continue;
        }
        if let Some(location) = panic_at(line) {
            let panic = Panic {
                location: location.to_owned(),
                message: String::new(),
            };
            let (owner, index) = match &mut active {
                Some((owner, index)) if *owner == job => (*owner, *index),
                _ => {
                    // A panic before any FAIL line still belongs to the job.
                    jobs[job].tests.push(TestFailure {
                        name: "unknown-test".into(),
                        duration: None,
                        panic: None,
                        lines: Vec::new(),
                    });
                    let index = jobs[job].tests.len() - 1;
                    active = Some((job, index));
                    (job, index)
                }
            };
            if jobs[owner].tests[index].panic.is_none() {
                jobs[owner].tests[index].panic = Some(panic);
            }
            continue;
        }
        if let Some(legacy) = legacy_panic(line) {
            if let Some((owner, index)) = active.filter(|(owner, _)| *owner == job)
                && jobs[owner].tests[index].panic.is_none()
            {
                jobs[owner].tests[index].panic = Some(legacy);
            } else {
                jobs[job].tests.push(TestFailure {
                    name: "unknown-test".into(),
                    duration: None,
                    panic: Some(legacy),
                    lines: Vec::new(),
                });
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let target = active
            .filter(|(owner, _)| *owner == job)
            .map(|(_, index)| index);
        if is_error_line(line) {
            match target.and_then(|index| jobs[job].tests.get_mut(index)) {
                Some(test) => test.lines.push(bounded_line(line)),
                None => push_error(&mut jobs[job], line),
            }
            continue;
        }
        if let Some(panic) = target.and_then(|index| jobs[job].tests[index].panic.as_mut())
            && captures_message(line)
        {
            if !panic.message.is_empty() {
                panic.message.push_str("; ");
            }
            panic.message.push_str(&bounded_line(line));
        }
    }
    jobs
}

fn push_error(job: &mut JobFailures, line: &str) {
    let line = bounded_line(line);
    if !job.errors.contains(&line) {
        job.errors.push(line);
    }
}

/// `gh run view --log-failed` prefixes every line with the job and step,
/// tab-separated; plain run logs have no prefix.
fn split_job(raw: &str) -> (&str, &str) {
    match raw.split_once('\t') {
        Some((job, rest)) => match rest.split_once('\t') {
            Some((_, line)) => (job, line),
            None => (job, rest),
        },
        None => ("run", raw),
    }
}

fn job_label(name: &str) -> String {
    if name.is_empty() {
        "run".into()
    } else {
        name.to_owned()
    }
}

/// GitHub Actions timestamps, when the raw log carries them.
fn strip_prefixes(line: &str) -> &str {
    let stamp = |line: &str| {
        let bytes = line.as_bytes();
        let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
        bytes.len() >= 20
            && digits(0..4)
            && bytes[4] == b'-'
            && digits(5..7)
            && bytes[7] == b'-'
            && digits(8..10)
            && bytes[10] == b'T'
            && digits(11..13)
            && bytes[13] == b':'
            && digits(14..16)
            && bytes[16] == b':'
            && digits(17..19)
    };
    if stamp(line)
        && let Some(at) = line.find('Z').filter(|at| *at < 40)
    {
        return line.get(at + 1..).map(str::trim_start).unwrap_or(line);
    }
    line
}

/// A nextest-style failing or timed-out test header.
fn fail_line(line: &str) -> Option<(String, Option<String>)> {
    let rest = line
        .strip_prefix("FAIL ")
        .or_else(|| line.strip_prefix("TIMEOUT "))?;
    let (duration, name) = match rest.strip_prefix('[') {
        Some(rest) => {
            let end = rest.find(']')?;
            (Some(rest[..end].trim().to_owned()), rest[end + 1..].trim())
        }
        None => {
            let (name, duration) = rest.split_once(" [")?;
            (duration.strip_suffix(']').map(str::to_owned), name.trim())
        }
    };
    let name = if name.starts_with('(') {
        name.split_once(')').map_or(name, |(_, rest)| rest.trim())
    } else {
        name
    };
    (!name.is_empty()).then(|| (name.to_owned(), duration))
}

/// A legacy `cargo test` failure line.
fn cargo_fail(line: &str) -> Option<(String, Option<String>)> {
    let name = line
        .strip_prefix("test ")?
        .strip_suffix(" ... FAILED")?
        .trim();
    (!name.is_empty()).then(|| (name.to_owned(), None))
}

/// Where a panic started: `panicked at <location>:` or a legacy single line.
fn panic_at(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("panicked at ")
        .or_else(|| line.split_once(" panicked at ").map(|(_, rest)| rest))?;
    let location = rest.strip_suffix(':')?;
    (!location.is_empty() && !location.contains('\'')).then_some(location)
}

/// A pre-1.73 panic line carries the message and location together.
fn legacy_panic(line: &str) -> Option<Panic> {
    let rest = line.strip_prefix("thread '")?;
    let (_, rest) = rest
        .split_once("' panicked at '")
        .or_else(|| rest.split_once("', panicked at '"))?;
    let (message, location) = rest.rsplit_once("', ")?;
    Some(Panic {
        message: message.to_owned(),
        location: location.trim_end_matches(':').to_owned(),
    })
}

/// Lines that continue a panic's message until the log moves on.
fn captures_message(line: &str) -> bool {
    !line.is_empty()
        && !line.starts_with("note:")
        && !line.starts_with("stack backtrace")
        && !line.starts_with("────")
        && !line.starts_with("error:")
        && !line.starts_with("error[")
        && !line.starts_with("warning:")
        && !line.starts_with("Summary [")
        && !line.starts_with("---")
        && fail_line(line).is_none()
}

fn is_error_line(line: &str) -> bool {
    line.starts_with("error:") || line.starts_with("error[") || line.starts_with("##[error]")
}

fn bounded_line(line: &str) -> String {
    provenant_cli::bounded_text(line, 512)
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "worktree driver includes this module without libtest"
)]
mod tests {
    use super::*;

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn captured_nextest_log_retains_failure_and_panic() {
        let jobs = parse_failed_log(include_str!("ci/fixtures/captured-nextest.log"));
        let job = &jobs[0];
        assert_eq!(job.tests.len(), 1);
        let failure = &job.tests[0];
        assert!(failure.name.ends_with(
            "native_shared_stores_keep_state_and_unlisted_files_denied_under_both_defaults"
        ));
        let panic = failure.panic.as_ref().expect("panic");
        assert!(panic.location.ends_with("host_boundaries.rs:399:17"));
        assert!(panic.message.contains("default=allow"));
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn cargo_failure_retains_test_panic_and_error() {
        let jobs = parse_failed_log(include_str!("ci/fixtures/cargo-failure.log"));
        let failure = jobs[0]
            .tests
            .iter()
            .find(|test| test.name == "cache_reads_expiry")
            .unwrap();
        let panic = failure.panic.as_ref().unwrap();
        assert!(panic.location.ends_with("src/cache.rs:64:5"));
        assert!(panic.message.contains("cannot lock cache"));
        assert!(
            failure
                .lines
                .iter()
                .chain(&jobs[0].errors)
                .any(|line| line.contains("test failed"))
        );
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn timeout_and_build_error_remain_visible() {
        let jobs = parse_failed_log(include_str!("ci/fixtures/nextest-failure.log"));
        assert!(
            jobs[0]
                .tests
                .iter()
                .any(|test| test.name.contains("lock_waits_for_reader"))
        );
        assert!(
            jobs[0]
                .tests
                .iter()
                .flat_map(|test| &test.lines)
                .chain(&jobs[0].errors)
                .any(|line| line.contains("error[E0308]"))
        );
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn terminal_escapes_do_not_hide_a_failing_test() {
        let raw = "\u{1b}[31mFAIL\u{1b}[0m [ 0.004s] ( 1/2) ci::tests::coloured_failure\n";
        let plain = strip_terminal_escapes(raw);
        assert_eq!(plain, "FAIL [ 0.004s] ( 1/2) ci::tests::coloured_failure\n");
        let jobs = parse_failed_log(&plain);
        assert_eq!(jobs[0].tests.len(), 1);
        assert_eq!(jobs[0].tests[0].name, "ci::tests::coloured_failure");
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn an_unreadable_log_keeps_the_failed_job_and_the_gh_message_visible() {
        let gh = "the response contains terminal escape sequences; pass --allow-escape-sequences to output it anyway";
        let report = Report {
            pr: Some(3727),
            run: None,
            head: None,
            runs: vec![RunFailures {
                run: 44_764_262_586,
                jobs: vec![JobFailures {
                    job: "Rust front door (1/3)".into(),
                    step: Some("Run native tests".into()),
                    class: JobClass::Failed,
                    unrelated: Vec::new(),
                    tests: Vec::new(),
                    errors: Vec::new(),
                    truncated: false,
                }],
                error: Some(format!(
                    "job Rust front door (1/3): unreadable log: external command failed: {gh}"
                )),
            }],
            unlogged: Vec::new(),
            rerun: None,
        };
        assert_eq!(real_jobs(&report.runs[0].jobs), 1);
        let text = human(&report);
        assert!(text.contains(gh), "{text}");
        assert!(text.contains("Rust front door (1/3)"), "{text}");
        assert!(
            rows(&report)
                .iter()
                .any(|row| row.contains("unreadable log"))
        );
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn job_log_with_nextest_failure_yields_test_and_panic() {
        let jobs = parse_failed_log(include_str!("ci/fixtures/job-nextest-failure.log"));
        let job = &jobs[0];
        assert_eq!(job.tests.len(), 1);
        let failure = &job.tests[0];
        assert!(failure.name.ends_with(
            "native_shared_stores_keep_state_and_unlisted_files_denied_under_both_defaults"
        ));
        let panic = failure.panic.as_ref().expect("panic");
        assert_eq!(
            panic.location,
            "crates/confinement/src/permissions/tests/host_boundaries.rs:399:17"
        );
        assert!(panic.message.contains("default=allow"));
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn aggregate_gate_failed_only_by_cancelled_members_is_cancelled() {
        let jobs = parse_jobs(include_str!("ci/fixtures/run-cancelled-aggregate.json").as_bytes())
            .expect("jobs fixture");
        let needs = parse_workflow_needs(include_str!("ci/fixtures/workflow-ci.yml"))
            .expect("workflow fixture")
            .expect("gate needs");
        let inputs = inputs_from_workflow(&needs, &jobs);
        assert!(inputs.complete, "fixture resolves every gate input");
        let classes = classify_jobs(&jobs, &inputs);
        let gate = jobs
            .iter()
            .find(|job| job.name == AGGREGATE_GATE)
            .expect("gate");
        assert_eq!(classes[&gate.id], JobClass::CancelledDependencies);
        assert!(classes[&gate.id].is_cancelled());
        let member = jobs
            .iter()
            .find(|job| job.name == "Rust front door (1/3)")
            .expect("member");
        assert_eq!(classes[&member.id], JobClass::Cancelled);
        // The unrelated cancelled job is cancelled in its own right, but it is
        // not one of the gate's inputs.
        let unrelated = jobs
            .iter()
            .find(|job| job.name == "Docs drift")
            .expect("unrelated job");
        assert_eq!(classes[&unrelated.id], JobClass::Cancelled);
    }

    // Regression for F13: an unrelated cancellation plus a missing required
    // member must never read as "the gate failed only because of cancellations".
    #[test]
    fn unrelated_cancelled_job_never_demotes_a_gate_with_a_missing_input() {
        let mut jobs =
            parse_jobs(include_str!("ci/fixtures/run-cancelled-aggregate.json").as_bytes())
                .expect("jobs fixture");
        jobs.retain(|job| job.name != "Rust front door (3/3)");
        assert!(jobs.iter().any(|job| job.name == "Docs drift"));
        let needs = parse_workflow_needs(include_str!("ci/fixtures/workflow-ci.yml"))
            .expect("workflow fixture")
            .expect("gate needs");
        let inputs = inputs_from_workflow(&needs, &jobs);
        assert!(!inputs.complete, "the missing matrix member is observable");
        let classes = classify_jobs(&jobs, &inputs);
        let gate = jobs
            .iter()
            .find(|job| job.name == AGGREGATE_GATE)
            .expect("gate");
        assert_eq!(classes[&gate.id], JobClass::Uncertain);
        assert!(!classes[&gate.id].is_cancelled());
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn only_the_gate_declared_members_decide_its_cancellation() {
        let mut jobs =
            parse_jobs(include_str!("ci/fixtures/run-cancelled-aggregate.json").as_bytes())
                .expect("jobs fixture");
        for job in &mut jobs {
            if job.name == "Docs drift" {
                job.conclusion = Some("failure".to_owned());
            }
        }
        let needs = parse_workflow_needs(include_str!("ci/fixtures/workflow-ci.yml"))
            .expect("workflow fixture")
            .expect("gate needs");
        let inputs = inputs_from_workflow(&needs, &jobs);
        assert!(inputs.complete);
        let classes = classify_jobs(&jobs, &inputs);
        let gate = jobs
            .iter()
            .find(|job| job.name == AGGREGATE_GATE)
            .expect("gate");
        assert_eq!(classes[&gate.id], JobClass::CancelledDependencies);
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn unknown_gate_inputs_keep_a_failed_gate_uncertain_but_failed() {
        let jobs = parse_jobs(
            br#"{"total_count":2,"jobs":[
                {"id":11,"name":"Docs drift","status":"completed","conclusion":"cancelled","steps":[]},
                {"id":12,"name":"ci-status","status":"completed","conclusion":"failure","steps":[]}
            ]}"#,
        )
        .expect("jobs");
        let classes = classify_jobs(&jobs, &GateInputs::unknown());
        assert_eq!(classes[&12], JobClass::Uncertain);
        assert!(!classes[&12].is_cancelled());
        assert!(class_label(JobClass::Uncertain).contains("unknown"));
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn gate_needs_published_on_the_jobs_endpoint_are_used() {
        let jobs = parse_jobs(
            br#"{"total_count":3,"jobs":[
                {"id":21,"name":"build","status":"completed","conclusion":"cancelled","steps":[]},
                {"id":22,"name":"Docs drift","status":"completed","conclusion":"cancelled","steps":[]},
                {"id":23,"name":"ci-status","status":"completed","conclusion":"failure","steps":[],"needs":["build"]}
            ]}"#,
        )
        .expect("jobs");
        let inputs = gate_inputs_from_jobs(&jobs).expect("needs from jobs API");
        assert!(inputs.complete);
        let classes = classify_jobs(&jobs, &inputs);
        assert_eq!(classes[&23], JobClass::CancelledDependencies);
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn an_unresolved_jobs_api_need_leaves_the_gate_uncertain() {
        let jobs = parse_jobs(
            br#"{"total_count":3,"jobs":[
                {"id":31,"name":"build","status":"completed","conclusion":"cancelled","steps":[]},
                {"id":32,"name":"Docs drift","status":"completed","conclusion":"cancelled","steps":[]},
                {"id":33,"name":"ci-status","status":"completed","conclusion":"failure","steps":[],"needs":["build","not-in-run"]}
            ]}"#,
        )
        .expect("jobs");
        let inputs = gate_inputs_from_jobs(&jobs).expect("needs from jobs API");
        assert!(!inputs.complete);
        let classes = classify_jobs(&jobs, &inputs);
        assert_eq!(classes[&33], JobClass::Uncertain);
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn a_matrix_declaration_expects_every_expanded_member() {
        let needs = parse_workflow_needs(include_str!("ci/fixtures/workflow-ci.yml"))
            .expect("workflow fixture")
            .expect("gate needs");
        let rust = needs
            .iter()
            .find(|need| need.key == "rust")
            .expect("rust need");
        assert_eq!(
            rust.patterns(),
            [
                "Rust front door (1/3)",
                "Rust front door (2/3)",
                "Rust front door (3/3)",
            ]
        );
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn matrix_display_name_templates_match_expanded_job_names() {
        let partition = "Rust front door (${{ matrix.partition }}/3)";
        assert!(name_matches(partition, "Rust front door (1/3)"));
        assert!(!name_matches(partition, "Rust front door (1/2)"));
        assert!(!name_matches(partition, "Rust front door (/3)"));
        let group = "Rust macOS tests (${{ matrix.group }})";
        assert!(name_matches(group, "Rust macOS tests (custody)"));
        assert!(!name_matches(group, "Rust macOS tests ()"));
        assert!(name_matches("Select CI suites", "Select CI suites"));
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn a_real_member_failure_keeps_the_aggregate_gate_a_failure() {
        let jobs = parse_jobs(
            br#"{"total_count":3,"jobs":[
                {"id":201,"name":"Rust Linux tests","status":"completed","conclusion":"failure","steps":[]},
                {"id":202,"name":"Rust macOS tests","status":"completed","conclusion":"cancelled","steps":[]},
                {"id":203,"name":"ci-status","status":"completed","conclusion":"failure","steps":[]}
            ]}"#,
        )
        .expect("jobs");
        let inputs = GateInputs {
            ids: vec![201, 202],
            complete: true,
        };
        let classes = classify_jobs(&jobs, &inputs);
        assert_eq!(classes[&201], JobClass::Failed);
        assert_eq!(classes[&202], JobClass::Cancelled);
        assert_eq!(classes[&203], JobClass::Failed);
        assert!(!classes[&203].is_cancelled());
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn a_non_gate_job_with_a_cancelled_sibling_is_never_demoted() {
        let jobs = parse_jobs(
            br#"{"total_count":2,"jobs":[
                {"id":301,"name":"Rust Linux tests","status":"completed","conclusion":"failure","steps":[]},
                {"id":302,"name":"Rust macOS tests","status":"completed","conclusion":"cancelled","steps":[]}
            ]}"#,
        )
        .expect("jobs");
        let classes = classify_jobs(&jobs, &GateInputs::unknown());
        assert_eq!(classes[&301], JobClass::Failed);
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn compact_rows_keep_a_non_test_error_with_its_job_step_and_class() {
        let report = Report {
            pr: Some(42),
            run: None,
            head: None,
            runs: vec![RunFailures {
                run: 7,
                jobs: vec![JobFailures {
                    job: "lint".into(),
                    step: Some("clippy".into()),
                    class: JobClass::Failed,
                    unrelated: Vec::new(),
                    tests: Vec::new(),
                    errors: vec!["error[E0308]: mismatched types".into()],
                    truncated: false,
                }],
                error: None,
            }],
            unlogged: Vec::new(),
            rerun: Some("provenant ci failures pr 42".into()),
        };
        let lines = rows(&report);
        let error_row = lines
            .iter()
            .find(|line| line.contains("mismatched types"))
            .expect("compact output kept the error");
        assert!(error_row.contains("lint"), "{error_row}");
        assert!(error_row.contains("clippy"), "{error_row}");
        assert!(error_row.contains("failed"), "{error_row}");
    }

    // Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn failure_in_untouched_crate_is_labelled_possibly_unrelated() {
        let changed = vec!["tools/front-door/crates/cache/src/store.rs".to_owned()];
        let jobs = parse_failed_log(include_str!("ci/fixtures/job-unrelated-failure.log"));
        let job = &jobs[0];
        assert_eq!(job.tests.len(), 2);
        let unrelated = unrelated_paths(job, &changed);
        assert_eq!(unrelated.len(), 1);
        assert_eq!(
            unrelated[0],
            "possibly unrelated: crates/confinement/src/permissions/tests/host_boundaries.rs untouched by this PR"
        );
        let touched = JobFailures {
            job: "cache".into(),
            step: None,
            class: JobClass::Failed,
            unrelated: Vec::new(),
            tests: job
                .tests
                .iter()
                .filter(|test| test.name.contains("reads_expired"))
                .cloned()
                .collect(),
            errors: Vec::new(),
            truncated: false,
        };
        assert!(unrelated_paths(&touched, &changed).is_empty());
    }
}
