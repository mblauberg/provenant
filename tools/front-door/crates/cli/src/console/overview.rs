//! Compact local menu read, composed from bounded owner batches.
use super::{Error, Project};
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, Roots, WaitBudget};
use provenant_os::clock::SystemClock;
use provenant_run::{ExecutionContext, OverviewState, RunStore};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Compare a chair's summary against the same bounded inbox projection used by
/// the overview. An incomplete scan cannot prove that a reference is stale.
pub fn summary_comparison(
    roots: &Roots,
    project: ProjectId,
    chair: &str,
    needs_you: &[String],
) -> Value {
    let unknown = || {
        needs_you
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    };
    let Some(host) = provenant_config::lookup_host(roots).ok().flatten() else {
        return json!({"stale":[],"missing":[],"unknown":unknown(),"coverage":"host_unavailable"});
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let Ok(budget) = WaitBudget::cli(clock.mono_now(), 5) else {
        return json!({"stale":[],"missing":[],"unknown":unknown(),"coverage":"unavailable"});
    };
    match provenant_fabric_runtime::inbox::overview(
        &roots.state,
        &clock,
        budget,
        &cancel,
        1_048_576,
    ) {
        Ok(page) => compare_summary(page, project, &host.to_string(), chair, needs_you),
        Err(provenant_fabric_runtime::inbox::OverviewError::Incompatible) => {
            json!({"stale":[],"missing":[],"unknown":unknown(),"coverage":"incompatible"})
        }
        Err(_) => json!({"stale":[],"missing":[],"unknown":unknown(),"coverage":"unavailable"}),
    }
}

fn compare_summary(
    page: provenant_fabric_runtime::inbox::OverviewBatch,
    project: ProjectId,
    host: &str,
    chair: &str,
    needs_you: &[String],
) -> Value {
    use std::collections::BTreeSet;
    let mut partial = page.truncated;
    let mut open = BTreeSet::new();
    let mut elsewhere = BTreeSet::new();
    let refs: BTreeSet<_> = needs_you.iter().cloned().collect();
    for item in page.items {
        if item.project_id != project.to_string() || item.chair_session_id.as_deref() != Some(chair)
        {
            if refs.contains(&item.id) {
                elsewhere.insert(item.id);
            }
            continue;
        }
        match item.owner_host.as_deref() {
            Some(owner) if provenant_core::HostId::parse(owner).is_ok() => {
                if owner == host {
                    open.insert(item.id);
                } else if refs.contains(&item.id) {
                    elsewhere.insert(item.id);
                }
            }
            _ => {
                partial = true;
                if refs.contains(&item.id) {
                    elsewhere.insert(item.id);
                }
            }
        }
    }
    // A reference seen under a different key cannot be attributed to this
    // chair, even when the scan itself is complete.
    let mut unknown = elsewhere;
    if partial {
        unknown.extend(refs.difference(&open).cloned());
    }
    let stale: Vec<_> = refs
        .difference(&open)
        .filter(|id| !unknown.contains(*id))
        .cloned()
        .collect();
    let missing: Vec<_> = open.difference(&refs).cloned().collect();
    partial |= !unknown.is_empty();
    json!({"stale":stale,"missing":missing,"unknown":unknown,"coverage":if partial { "partial" } else { "complete" }})
}

pub(super) fn read(roots: &Roots, projects: &[Project]) -> Result<Value, Error> {
    const BYTE_BUDGET: usize = 1_048_576;
    let labels: BTreeMap<_, _> = projects
        .iter()
        .map(|p| {
            let path = p.common_dir["path"].as_str();
            let label = (p.common_dir["encoding"] == "utf8")
                .then_some(path)
                .flatten()
                .and_then(|p| std::path::Path::new(p).parent())
                .and_then(|p| p.file_name())
                .and_then(|p| p.to_str())
                .map(str::to_owned);
            (p.project_id.to_string(), label)
        })
        .collect();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).map_err(|_| Error::Encode)?;
    let mut needs_you = Vec::new();
    let mut remaining_bytes = BYTE_BUDGET;
    let fabric = match provenant_fabric_runtime::inbox::overview(
        &roots.state,
        &clock,
        budget,
        &cancel,
        remaining_bytes,
    ) {
        Ok(page) => {
            remaining_bytes -= page.bytes_read;
            for item in page.items {
                if let Some(label) = labels.get(&item.project_id) {
                    needs_you.push(json!({"id":item.id,"project_id":item.project_id,"project_label":label,"title":item.title,"task_label":null,"chair_session_id":item.chair_session_id}));
                }
            }
            if page.truncated {
                "partial"
            } else {
                "complete"
            }
        }
        Err(provenant_fabric_runtime::inbox::OverviewError::Incompatible) => "incompatible",
        Err(_) => "unavailable",
    };
    let run_call = provenant_run::Request {
        clock: &clock,
        budget,
        cancel: &cancel,
    };
    let context = ExecutionContext {
        state_root: roots.state.clone(),
        project_id: projects
            .first()
            .map_or(ProjectId::from_bytes([0; 16]), |p| p.project_id),
        actor_id: ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    let mut lanes = Vec::new();
    let (mut running, mut attention) = (0usize, 0usize);
    let run = match RunStore::open_read_only(&context, &run_call) {
        Ok(Some(store)) => match store.overview_attempts(128, remaining_bytes, &run_call) {
            Ok(batch) => {
                for item in batch.attempts {
                    let Some(label) = labels.get(&item.project_id.to_string()) else {
                        continue;
                    };
                    let (state, active, needs_attention) = match item.state {
                        OverviewState::Queued => ("queued", true, false),
                        OverviewState::Starting => ("starting", true, false),
                        OverviewState::Running => ("running", true, false),
                        OverviewState::Settling => ("settling", true, false),
                        OverviewState::Terminal(outcome) => {
                            use provenant_run::Outcome;
                            let attention =
                                !matches!(outcome, Outcome::Succeeded | Outcome::Cancelled);
                            ("terminal", false, attention)
                        }
                    };
                    running += usize::from(active);
                    attention += usize::from(needs_attention);
                    if active || needs_attention {
                        lanes.push(json!({"project_id":item.project_id,"project_label":label,"run_id":item.run_id,"task_id":item.task_id,"attempt_id":item.attempt_id,"task_label":item.title,"state":state,"attention":needs_attention}));
                    }
                }
                if batch.truncated {
                    "partial"
                } else {
                    "complete"
                }
            }
            Err(_) => "unavailable",
        },
        Err(provenant_run::RunError::Version(_)) => "incompatible",
        _ => "unavailable",
    };
    Ok(json!({"needs_you":needs_you,"lanes":lanes,
        "counts":{"needs_you":(fabric != "unavailable" && fabric != "incompatible").then_some(needs_you.len()),
                  "running":(run != "unavailable" && run != "incompatible").then_some(running),
                  "attention":(run != "unavailable" && run != "incompatible").then_some(attention)},
        "coverage":{"scope":"local","inbox":fabric,"runs":run,"nested_runs":"unavailable"}}))
}

#[cfg(test)]
mod comparison_tests {
    use super::*;
    use provenant_fabric_runtime::inbox::{OverviewBatch, OverviewItem};

    #[test]
    fn compares_only_matching_host_project_and_chair_and_respects_partial_coverage() {
        let project = ProjectId::from_bytes([1; 16]);
        let host = provenant_core::HostId::from_bytes([2; 16]).to_string();
        let foreign = provenant_core::HostId::from_bytes([3; 16]).to_string();
        let item =
            |id: &str, owner: Option<String>, session: &str, project_id: String| OverviewItem {
                id: id.into(),
                project_id,
                title: String::new(),
                chair_session_id: Some(session.into()),
                owner_host: owner,
            };
        let page = |truncated| OverviewBatch {
            items: vec![
                item("open", Some(host.clone()), "chair", project.to_string()),
                item(
                    "foreign",
                    Some(foreign.clone()),
                    "chair",
                    project.to_string(),
                ),
                item("other", Some(host.clone()), "other", project.to_string()),
                item(
                    "project",
                    Some(host.clone()),
                    "chair",
                    ProjectId::from_bytes([4; 16]).to_string(),
                ),
            ],
            truncated,
            bytes_read: 0,
        };
        let refs = vec![
            "closed".into(),
            "foreign".into(),
            "other".into(),
            "project".into(),
        ];
        let complete = compare_summary(page(false), project, &host, "chair", &refs);
        assert_eq!(complete["stale"], json!(["closed"]));
        assert_eq!(complete["missing"], json!(["open"]));
        assert_eq!(complete["unknown"], json!(["foreign", "other", "project"]));
        assert_eq!(complete["coverage"], "partial");
        let partial = compare_summary(page(true), project, &host, "chair", &refs);
        assert_eq!(partial["stale"], json!([]));
        assert_eq!(partial["missing"], json!(["open"]));
        assert_eq!(
            partial["unknown"],
            json!(["closed", "foreign", "other", "project"])
        );
        assert_eq!(partial["coverage"], "partial");
        let clean = compare_summary(
            page(false),
            project,
            &host,
            "chair",
            &["open".into(), "closed".into()],
        );
        assert_eq!(clean["stale"], json!(["closed"]));
        assert_eq!(clean["unknown"], json!([]));
        assert_eq!(clean["coverage"], "complete");
        let mut unknown = page(false);
        unknown
            .items
            .push(item("unknown", None, "chair", project.to_string()));
        let unknown = compare_summary(unknown, project, &host, "chair", &["unknown".into()]);
        assert_eq!(unknown["coverage"], "partial");
        assert_eq!(unknown["unknown"], json!(["unknown"]));
        assert_eq!(unknown["stale"], json!([]));
    }
}
