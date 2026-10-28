//! Compact local menu read, composed from bounded owner batches.
use super::{Error, Project};
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, Roots, WaitBudget};
use provenant_os::clock::SystemClock;
use provenant_run::{ExecutionContext, OverviewState, RunStore};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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
                    needs_you.push(json!({"id":item.id,"project_id":item.project_id,"project_label":label,"title":item.title,"task_label":null}));
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
