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
        provenant_cli::Ci::Failures { target } => match target {
            provenant_cli::ci::FailureTarget::Pr { number } => failures_pr(number, globals),
            provenant_cli::ci::FailureTarget::Run { run } => failures_run(run, globals),
        },
    }
}

/// One failed job's evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JobFailures {
    pub job: String,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunFailures {
    pub run: u64,
    pub jobs: Vec<JobFailures>,
    /// The run's failed-job logs could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn failures_pr(number: u64, globals: &Globals) -> u8 {
    const LEAF: &str = "ci failures";
    let result = (|| -> Result<Report, Box<dyn std::error::Error>> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let state = roots::resolve(globals, roots::Workspace::Project, roots::Product::Optional)?
            .roots
            .state;
        let request = provenant_config::ports::Request::within(READ_BUDGET);
        // The batched read is also the REST fallback's quota escape: whichever
        // transport answers first resolves the head and its failed checks.
        let batch_result = batch::single(state.as_path(), &project, number);
        let try_graphql = !matches!(batch_result, batch::Batch::RateLimited(_));
        let (head, repository, checks) = match batch_result {
            batch::Batch::Entries(entries)
                if !entries.is_empty()
                    && !entries[0].truncated
                    && entries[0].state != "UNAVAILABLE" =>
            {
                let entry = &entries[0];
                (
                    entry.head.clone(),
                    repository_of(&entry.url),
                    entry.checks.clone(),
                )
            }
            _ => resolve_pr_rest(state.as_path(), &project, number, &request, try_graphql)?,
        };
        let Some(repository) = repository else {
            return Err(
                format!("pull request {number} names no repository to read logs from").into(),
            );
        };
        report(
            state.as_path(),
            &project,
            &repository,
            Some(number),
            Some(head),
            &checks,
            &request,
        )
    })();
    emit(LEAF, result, globals)
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
        let logs = failed_logs(state.as_path(), &project, &repository, run, &request);
        let jobs = logs.as_deref().map(parse_failed_log).unwrap_or_default();
        Ok(Report {
            pr: None,
            run: Some(run),
            head: None,
            runs: vec![RunFailures {
                run,
                jobs: compact(jobs),
                error: logs.err().map(|error| bounded(&error)),
            }],
            unlogged: Vec::new(),
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
        url: String,
    }
    let resolved = match successful(forge_execute(
        state,
        project,
        &[
            "gh",
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "headRefOid,url",
        ],
        request,
        false,
        None,
    )?) {
        Ok(bytes) => match serde_json::from_slice::<View>(&bytes) {
            Ok(view) => Ok(view),
            Err(error) => Err(error.to_string()),
        },
        Err(error) if try_graphql && rate_limited(error.as_ref()) => {
            match batch::single(state, project, number) {
                batch::Batch::Entries(entries) if !entries.is_empty() && !entries[0].truncated => {
                    return Ok((
                        entries[0].head.clone(),
                        repository_of(&entries[0].url),
                        entries[0].checks.clone(),
                    ));
                }
                _ => Err(format!(
                    "pull request {number} is rate limited and the batched read could not answer"
                )),
            }
        }
        Err(error) => Err(error.to_string()),
    };
    let view = resolved.map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let repository = repository_of(&view.url).ok_or("pull request URL names no repository")?;
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
fn rate_limited(error: &dyn std::error::Error) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("rate limit")
}

/// The base repository a pull request URL names, whose CI the checks ran on.
fn repository_of(url: &str) -> Option<String> {
    let segments: Vec<&str> = url.trim_end_matches('/').split('/').collect();
    let [.., owner, name, "pull", _] = segments.as_slice() else {
        return None;
    };
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
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
        let logs = failed_logs(state, project, repository, run, request);
        let jobs = logs.as_deref().map(parse_failed_log).unwrap_or_default();
        runs.push(RunFailures {
            run,
            jobs: compact(jobs),
            error: logs.err().map(|error| bounded(&error)),
        });
    }
    Ok(Report {
        pr,
        run: None,
        head,
        runs,
        unlogged,
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

/// The failed-job logs of one run, cached for the fast observation interval:
/// a completed run's logs never change, and a still-running run gains them.
fn failed_logs(
    state: &Path,
    project: &Path,
    repository: &str,
    run: u64,
    request: &provenant_config::ports::Request,
) -> Result<String, String> {
    let dir = crate::pr::observer::shared_dir(state, repository)
        .map_err(|error| format!("cannot open shared CI cache: {error}"))?;
    let name = format!("run-failures-{run}.json");
    let ttl = crate::pr::observer::fast_interval(state, project)
        .unwrap_or(std::time::Duration::from_secs(5));
    let now = clock();
    #[derive(Serialize, Deserialize)]
    struct Cached {
        fetched_at: i64,
        text: String,
    }
    if let Ok(bytes) = dir.read_bounded(std::ffi::OsStr::new(&name), 4 * 1024 * 1024)
        && let Ok(cached) = serde_json::from_slice::<Cached>(&bytes)
    {
        let age = now.as_millisecond().saturating_sub(cached.fetched_at);
        if (0..i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX)).contains(&age) {
            return Ok(cached.text);
        }
    }
    let bytes = successful(
        forge_execute(
            state,
            project,
            &[
                "gh",
                "run",
                "view",
                &run.to_string(),
                "-R",
                repository,
                "--log-failed",
            ],
            request,
            false,
            None,
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if let Ok(bytes) = serde_json::to_vec(&Cached {
        fetched_at: now.as_millisecond(),
        text: text.clone(),
    }) {
        let _ = dir.atomic_write(
            std::ffi::OsStr::new(&name),
            ".run-failures-",
            &bytes,
            provenant_os::fs::PublishMode::Replace,
        );
    }
    Ok(text)
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
            let failing_jobs = report.runs.iter().map(|run| run.jobs.len()).sum::<usize>();
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
            let next = report
                .runs
                .first()
                .map(|run| format!("gh run view {} --log-failed", run.run));
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
            globals,
        ),
    }
}

fn rows(report: &Report) -> Vec<String> {
    let mut rows = Vec::new();
    for run in &report.runs {
        for job in &run.jobs {
            for test in &job.tests {
                let mut row = format!("run {} {}: {}", run.run, job.job, test.name);
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
        }
    }
    for unlogged in &report.unlogged {
        rows.push(format!("check {}: no Actions run logs", unlogged.check));
    }
    rows
}

/// The compact default text: one row per failing test, then error lines.
fn human(report: &Report) -> String {
    let mut text = String::new();
    for run in &report.runs {
        if let Some(error) = &run.error {
            text.push_str(&format!("run {}: unreadable: {error}\n", run.run));
            continue;
        }
        for job in &run.jobs {
            text.push_str(&format!(
                "run {} job {}: {} failing test(s)\n",
                run.run,
                job.job,
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
    line.starts_with("error:") || line.starts_with("error[")
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
}
