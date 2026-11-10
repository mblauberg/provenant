//! `telemetry backfill`: copy every terminal attempt in the format-1 run store
//! into the usage ledger, so lane usage outlives the store. The ledger keeps
//! one row per attempt, so running it again adds nothing.
use super::*;
use provenant_core::ProjectId;
use provenant_telemetry::ledger;

pub fn backfill(edge: &Edge<'_>) -> Reply {
    reply("telemetry backfill", backfill_inner(edge))
}

fn backfill_inner(edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    use run::AttemptReader;
    let prices = super::telemetry::price_table(edge.assets)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let context = |project_id| run::ExecutionContext {
        state_root: edge.roots.state.clone(),
        project_id,
        // Reads never act; no actor is attributed.
        actor_id: ActorId::from_bytes([0; 16]),
        read_prompts: false,
    };
    // The store lists every project's attempts whichever project opens it.
    let projects = run::RunStore::open_read_only(&context(ProjectId::from_bytes([0; 16])), &call)?
        .map(|store| store.terminal_projects(&call))
        .transpose()?
        .unwrap_or_default();
    let mut rows = Vec::new();
    for project in &projects {
        let Some(store) = run::RunStore::open_read_only(&context(*project), &call)? else {
            continue;
        };
        for fact in store.attempts(run::AttemptFilter::default(), &call)? {
            let fact = fact?;
            let run_id = store.record(fact.attempt_id, &call)?.run_id;
            rows.push(
                ledger::row(&fact, Some(run_id), ledger::Source::Backfill, Some(&prices)).map_err(
                    |error| Fail::new(Status::Error, ErrorKind::Invalid, error.to_string()),
                )?,
            );
        }
    }
    let io =
        |error: ledger::LedgerError| Fail::new(Status::Error, ErrorKind::Io, error.to_string());
    let appended =
        ledger::append(edge.roots.state.as_path(), &rows, &clock, &cancel).map_err(io)?;
    let held = ledger::read(edge.roots.state.as_path()).map_err(io)?;
    let mut human = format!(
        "Backfilled {} attempts from {} projects: {} added, {} already in the ledger.\nThe usage ledger holds {} rows.\n",
        rows.len(),
        projects.len(),
        appended.added,
        appended.present,
        held.rows.len()
    );
    for item in &held.unrecognised {
        human.push_str(&format!(
            "Unrecognised ledger line {}\n",
            plain_text(&item.to_string(), 512)
        ));
    }
    let data = json!({
        "projects": projects.len(),
        "attempts": rows.len(),
        "added": appended.added,
        "present": appended.present,
        "ledger_rows": held.rows.len(),
        "unrecognised": held.unrecognised,
    });
    Ok((data, human))
}
