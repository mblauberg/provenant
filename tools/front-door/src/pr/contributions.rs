//! Host-only capture and retained contribution reader. Never consult peer stores.
use super::{Result, execute, successful};
use crate::{emit, fail, roots};
use provenant_cli::Globals;
use provenant_core::{CancelToken, Clock, GitOid, ProjectId, WaitBudget};
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
                #[derive(serde::Deserialize)]
                struct State {
                    state: String,
                }
                let bytes = successful(execute(
                    &self.resolved.project,
                    &[
                        "gh",
                        "pr",
                        "view",
                        &candidate.number.to_string(),
                        "--repo",
                        &candidate.repository,
                        "--json",
                        "state",
                    ],
                    false,
                )?)?;
                let state = serde_json::from_slice::<State>(&bytes)?.state;
                if matches!(state.as_str(), "CLOSED" | "MERGED") {
                    db.close(project, &candidate, &call)?;
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
            Contributions::open(self.resolved.roots.state.as_path(), &call)?.close(
                project,
                &self.pr(number),
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
            "local_attempts: {coverage}\nskipped_sealed: {skipped_sealed}\n"
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
            "coverage":{"local_attempts":coverage, "ambiguous":ambiguous,"truncated":truncated,"skipped_sealed":skipped_sealed, "peer_attempts":"unknown","known_peer_hosts":peers}}),
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
