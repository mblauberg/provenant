//! Host-only capture and retained contribution reader. Never consult peer stores.
use super::{Result, execute, successful};
use crate::{emit, fail, roots};
use provenant_cli::Globals;
use provenant_core::{CancelToken, Clock, GitOid, ProjectId, WaitBudget, WallTime};
use provenant_os::clock::SystemClock;
use provenant_records::{CommandResult, Outcome};
use provenant_run::{Contributions, PullRequest, Request};
use serde_json::json;
use std::{path::Path, time::Duration};

fn repository(project: &Path) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Repository {
        #[serde(rename = "nameWithOwner")]
        name: String,
    }
    let bytes = successful(execute(
        project,
        &["gh", "repo", "view", "--json", "nameWithOwner"],
        false,
    )?)?;
    Ok(serde_json::from_slice::<Repository>(&bytes)?
        .name
        .to_ascii_lowercase())
}

fn project_id(resolved: &roots::Resolved) -> Result<Option<ProjectId>> {
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let runtime = provenant_confinement::UtilityRuntime::read_only(git)?;
    let location = runtime
        .project_location(
            &provenant_os::exec::SystemExecutor::new(1)?,
            &resolved.project,
            provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
            &CancelToken::new(),
        )?
        .location;
    Ok(
        provenant_config::lookup_project(&resolved.roots, &location)?
            .map(|binding| binding.project_id()),
    )
}

/// One command owns one repository lookup, including bind and close retries.
pub(super) struct Tracker {
    resolved: roots::Resolved,
    project: Option<ProjectId>,
    repository: String,
}

impl Tracker {
    pub(super) fn resolve(globals: &Globals) -> Result<Self> {
        Self::new(roots::resolve(
            globals,
            roots::Workspace::Checkout,
            roots::Product::Optional,
        )?)
    }

    pub(super) fn new(resolved: roots::Resolved) -> Result<Self> {
        Ok(Self {
            project: project_id(&resolved)?,
            repository: repository(&resolved.project)?,
            resolved,
        })
    }

    fn pr(&self, number: u64) -> PullRequest {
        PullRequest {
            repository: self.repository.clone(),
            number,
        }
    }

    pub(super) fn bind(&self, branch: &str, number: u64, head: GitOid) -> Result<()> {
        let project = self
            .project
            .ok_or("project is unbound; register the project before binding contributions")?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
        };
        let db = Contributions::open(self.resolved.roots.state.as_path(), &call)?;
        let pr = self.pr(number);
        for candidate in db.open_bindings(project, branch, &call)? {
            if candidate == pr {
                continue;
            }
            let swept = (|| -> Result<()> {
                let (state, ended_at) = self.observed_end(&candidate)?;
                if matches!(state.as_str(), "CLOSED" | "MERGED") {
                    db.close_at(project, &candidate, ended_at, &call)?;
                }
                Ok(())
            })();
            if let Err(error) = swept {
                warn(
                    Some(candidate.number),
                    &format!("stale binding sweep failed: {error}"),
                );
            }
        }
        db.bind(project, branch, &pr, head, &call)?;
        Ok(())
    }

    /// GitHub state and end time of a PR from one gh call.
    fn observed_end(&self, pr: &PullRequest) -> Result<(String, Option<WallTime>)> {
        let bytes = successful(execute(
            &self.resolved.project,
            &[
                "gh",
                "pr",
                "view",
                &pr.number.to_string(),
                "--repo",
                &pr.repository,
                "--json",
                "state,mergedAt,closedAt",
            ],
            false,
        )?)?;
        parse_end(&bytes)
    }

    pub(super) fn close(&self, number: u64) -> Result<()> {
        let project = self
            .project
            .ok_or("project is unbound; register the project before binding contributions")?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
        };
        // A status observation must never create a store.
        let closed = if Contributions::open_read_only(self.resolved.roots.state.as_path(), &call)?
            .is_some()
        {
            // An unreadable end time still closes the binding, without reassignment.
            let ended_at = self
                .observed_end(&self.pr(number))
                .map(|(_, ended_at)| ended_at)
                .unwrap_or_else(|error| {
                    warn(Some(number), &format!("PR end time unavailable: {error}"));
                    None
                });
            Contributions::open(self.resolved.roots.state.as_path(), &call)?.close_at(
                project,
                &self.pr(number),
                ended_at,
                &call,
            )?
        } else {
            false
        };
        if !closed {
            warn(
                Some(number),
                &"no open contribution binding to close; the binding is missing or was already closed",
            );
        }
        Ok(())
    }
}

/// Read the repository's merged PR snapshot for telemetry grouping.
pub(crate) fn merged_prs(
    globals: &Globals,
    since: Option<jiff::Timestamp>,
    until: Option<jiff::Timestamp>,
) -> Result<Vec<PullRequest>> {
    #[derive(serde::Deserialize)]
    struct Merged {
        number: u64,
        #[serde(rename = "mergedAt")]
        merged_at: Option<String>,
    }
    let tracker = Tracker::resolve(globals)?;
    let mut argv = vec![
        "gh".to_owned(),
        "pr".into(),
        "list".into(),
        "--repo".into(),
        tracker.repository.clone(),
        "--state".into(),
        "merged".into(),
        "--json".into(),
        "number,mergedAt".into(),
        "--limit".into(),
        "1001".into(),
    ];
    let mut search = Vec::new();
    if let Some(since) = since {
        search.push(format!("merged:>={}", since.strftime("%Y-%m-%d")));
    }
    if let Some(until) = until {
        search.push(format!("merged:<={}", until.strftime("%Y-%m-%d")));
    }
    if !search.is_empty() {
        argv.extend(["--search".into(), search.join(" ")]);
    }
    let args = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let bytes = successful(execute(&tracker.resolved.project, &args, false)?)?;
    let rows: Vec<Merged> = serde_json::from_slice(&bytes)?;
    if rows.len() > 1000 {
        return Err("merged PR snapshot exceeds 1,000 rows; narrow the time window".into());
    }
    let mut merged_prs = Vec::new();
    for row in rows {
        let raw = row.merged_at.ok_or("merged PR has no merge timestamp")?;
        let merged_at = raw.parse::<jiff::Timestamp>()?;
        if since.is_none_or(|since| merged_at >= since)
            && until.is_none_or(|until| merged_at < until)
        {
            merged_prs.push(PullRequest {
                repository: tracker.repository.clone(),
                number: row.number,
            });
        }
    }
    Ok(merged_prs)
}

/// GitHub reports whole seconds, and the instant is compared with the local
/// clock that stamped `queued_at`, so skew can misplace attempts near the boundary.
fn parse_end(bytes: &[u8]) -> Result<(String, Option<WallTime>)> {
    #[derive(serde::Deserialize)]
    struct State {
        state: String,
        #[serde(rename = "mergedAt")]
        merged_at: Option<String>,
        #[serde(rename = "closedAt")]
        closed_at: Option<String>,
    }
    let view = serde_json::from_slice::<State>(bytes)?;
    let ended_at = match view.merged_at.or(view.closed_at) {
        Some(text) => {
            let stamp: jiff::Timestamp = text.parse()?;
            Some(WallTime::from_micros(stamp.as_microsecond())?)
        }
        None => None,
    };
    Ok((view.state, ended_at))
}

/// Workflow success must not depend on optional attribution storage.
pub(super) fn warn(number: Option<u64>, error: &dyn std::fmt::Display) {
    use std::io::Write;
    let number = number.map_or_else(|| "N".into(), |number| number.to_string());
    let _ = writeln!(
        std::io::stderr().lock(),
        "warning: PR contribution binding failed: {error}; retry `provenant pr contributions {number} --bind`"
    );
}

pub(super) fn run(number: u64, bind_requested: bool, globals: &Globals) -> u8 {
    const LEAF: &str = "pr contributions";
    let result = (|| -> Result<_> {
        let resolved = roots::resolve(
            globals,
            roots::Workspace::Checkout,
            roots::Product::Optional,
        )?;
        let tracker = Tracker::new(resolved)?;
        let resolved = &tracker.resolved;
        let pr = tracker.pr(number);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
        };
        let project_bound = tracker.project.is_some();
        if bind_requested {
            if !project_bound {
                return Err(
                    "project is unbound; register the project before binding contributions".into(),
                );
            }
            let view = super::land::pull(&resolved.project, &number.to_string())?;
            if view.cross_repository {
                return Err("cannot bind a fork PR to a local namesake branch".into());
            }
            let bound = tracker.bind(
                &view.branch,
                number,
                super::head(&resolved.project, &number.to_string())?,
            );
            let closed = if matches!(view.state.as_str(), "CLOSED" | "MERGED") {
                tracker.close(number)
            } else {
                Ok(())
            };
            bound.and(closed)?;
        }
        let db = Contributions::open_read_only(resolved.roots.state.as_path(), &call)?;
        let attempts = db
            .as_ref()
            .map(|db| db.for_pr(&pr, &call))
            .transpose()?
            .unwrap_or_default();
        let truncated = db
            .as_ref()
            .map(|db| db.truncated_for_pr(&pr, &call))
            .transpose()?
            .unwrap_or(false);
        let skipped_sealed = db
            .as_ref()
            .map(|db| db.skipped_sealed_for_pr(&pr, &call))
            .transpose()?
            .unwrap_or(0);
        let reassigned = db
            .as_ref()
            .map(|db| db.reassigned_from_pr(&pr, &call))
            .transpose()?
            .unwrap_or(0);
        let unplaced = db
            .as_ref()
            .map(|db| db.unplaced_for_pr(&pr, &call))
            .transpose()?
            .unwrap_or(0);
        let ambiguous = attempts.iter().any(|row| row.ambiguous);
        let bound = project_bound
            && db
                .as_ref()
                .map(|db| db.is_bound(&pr, &call))
                .transpose()?
                .unwrap_or(false);
        let coverage = if !bound {
            "unbound"
        } else if truncated {
            "truncated"
        } else if skipped_sealed > 0 {
            "skipped"
        } else if ambiguous {
            "ambiguous"
        } else {
            "captured"
        };
        let config = provenant_config::read_with_domains(
            &resolved.roots,
            &provenant_cli::domains::SECTIONS,
        )?;
        let local = provenant_config::lookup_host(&resolved.roots)?;
        let peers: Vec<_> = config
            .domains
            .fabric
            .peers
            .keys()
            .cloned()
            .chain(
                config
                    .domains
                    .fabric
                    .home_hosts
                    .iter()
                    .filter(|(_, host)| Some(**host) != local)
                    .map(|(name, _)| name.clone()),
            )
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut text = format!(
            "{} #{}: {} local attempts\n",
            pr.repository,
            number,
            attempts.len()
        );
        for attempt in &attempts {
            let weight = attempt
                .weight_millionths
                .map_or_else(|| "unknown".into(), |value| format!("{value}/1000000"));
            text.push_str(&format!(
                "{} role={} model={} account={} weight={} run={} outcome={} queued_at={} finished_at={} pr_count={} ambiguous={} truncated={}\n",
                attempt.attempt,
                attempt.role.as_deref().unwrap_or("unknown"),
                attempt.model.as_deref().unwrap_or("unknown"),
                attempt.account.as_deref().unwrap_or("unknown"),
                weight,
                attempt.run.map_or_else(|| "unknown".into(), |id| id.to_string()),
                attempt.outcome.map_or_else(|| "unknown".into(), |outcome| serde_json::to_value(outcome).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_else(|| "unknown".into())),
                attempt.queued_at.map_or_else(|| "unknown".into(), |time| time.to_string()),
                attempt.finished_at.map_or_else(|| "unknown".into(), |time| time.to_string()),
                attempt.pr_count, attempt.ambiguous, attempt.truncated
            ));
        }
        text.push_str(&format!(
            "local_attempts: {coverage}\nskipped_sealed: {skipped_sealed}\nreassigned: {reassigned}\nunplaced: {unplaced}\n"
        ));
        text.push_str(&format!(
            "peer_attempts: unknown (known hosts: {})\n",
            if peers.is_empty() {
                "unknown".into()
            } else {
                peers.join(", ")
            }
        ));
        Ok((
            json!({"pull_request":pr,"attempts":attempts,
            "coverage":{"local_attempts":coverage, "ambiguous":ambiguous,"truncated":truncated,"skipped_sealed":skipped_sealed,"reassigned":reassigned,"unplaced":unplaced, "peer_attempts":"unknown","known_peer_hosts":peers}}),
            text,
        ))
    })();
    match result {
        Ok((data, text)) => emit(
            &CommandResult::new(LEAF, Outcome::Ok, data),
            globals,
            &text,
            true,
        ),
        Err(error) => fail(
            LEAF,
            Outcome::Error,
            "PR_CONTRIBUTIONS",
            &error.to_string(),
            globals,
        ),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn end_time_prefers_merge_and_tolerates_nulls() {
        let (state, at) = super::parse_end(br#"{"state":"MERGED","mergedAt":"2026-01-02T03:04:05Z","closedAt":"2026-01-02T03:04:06Z"}"#).unwrap();
        assert_eq!(state, "MERGED");
        assert_eq!(at.unwrap().as_micros(), 1_767_323_045_000_000);
        let (_, at) = super::parse_end(
            br#"{"state":"CLOSED","mergedAt":null,"closedAt":"2026-01-02T03:04:06Z"}"#,
        )
        .unwrap();
        assert_eq!(at.unwrap().as_micros(), 1_767_323_046_000_000);
        let (_, at) =
            super::parse_end(br#"{"state":"CLOSED","mergedAt":null,"closedAt":null}"#).unwrap();
        assert!(at.is_none());
    }
}
