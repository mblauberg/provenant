use super::*;

/// Retain a content-addressed preset graph in the same verified scratch area
/// consumed by the graph owner. Repeated publication must match byte for byte.
pub(crate) fn retain_preset_graph(
    document: &serde_json::Value,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let factory = factory(globals, workers)?;
    if factory.context.in_lane {
        return Err(CoordinationError::LaneMutation.into());
    }
    let cancel = CancelToken::new();
    let scratch = provenant_confinement::worktrees::with_context(
        &factory.context.roots,
        &factory.home,
        &factory.cwd,
        &factory.clock,
        &cancel,
        |context| {
            let repo = context.utility.open_repo(
                context.executor,
                &factory.cwd,
                context.utility_budget,
                &cancel,
            )?;
            provenant_confinement::scratch::ensure(&repo, &factory.clock, &cancel)
        },
    )?;
    publish_preset_graph(&scratch, document)
}

pub(crate) fn publish_preset_graph(
    scratch: &std::path::Path,
    document: &serde_json::Value,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use provenant_os::fs::{Directory, PublishMode};
    use std::ffi::OsStr;
    let bytes = serde_json::to_vec(document)?;
    let digest = provenant_records::CanonicalV1::file_digest(&bytes).to_string();
    let filename = format!("preset-{}.json", &digest[7..]);
    let dir = Directory::open_anchor(&scratch.join("graphs"))?;
    let directory_metadata = dir.metadata()?;
    if directory_metadata.uid != provenant_os::fs::effective_uid()
        || directory_metadata.mode & 0o077 != 0
    {
        return Err("preset graph directory must be private to the current user".into());
    }
    match dir.atomic_write(
        OsStr::new(&filename),
        ".preset-",
        &bytes,
        PublishMode::NoReplace,
    ) {
        Ok(publication) => publication.require_durable()?,
        Err(error) if error.is_exists() => {
            let metadata = dir.metadata_at(OsStr::new(&filename), false)?;
            if metadata.uid != provenant_os::fs::effective_uid()
                || metadata.mode & 0o077 != 0
                || metadata.links != 1
                || dir.read_stable(OsStr::new(&filename), 8_388_608)? != bytes
            {
                return Err(
                    "existing preset graph is not private or does not match its content hash"
                        .into(),
                );
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(scratch.join("graphs").join(filename))
}

/// Preset waiting advances the retained immutable graph, including CI gates.
pub(crate) fn run_preset_graph(
    document: PathBuf,
    wait_seconds: u64,
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    use provenant_fabric_runtime::graph::GraphStatus;
    use provenant_records::Outcome;
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
        if factory.context.in_lane {
            return Err(CoordinationError::LaneMutation.into());
        }
        let cancel = CancelToken::new();
        let waiting = WaitBudget::cli(factory.clock.mono_now(), wait_seconds)?;
        loop {
            let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
            let run = factory.graph(
                GraphInput::Run {
                    extra: Default::default(),
                    document: document.clone(),
                },
                budget,
                &cancel,
            )?;
            if run.status != GraphStatus::Pending
                || waiting.remaining(factory.clock.mono_now()).as_micros() == 0
            {
                return Ok(run);
            }
            provenant_core::Pause::wait(
                &factory.backoff,
                provenant_core::DurationMicros::from_micros(
                    250_000.min(waiting.remaining(factory.clock.mono_now()).as_micros()),
                ),
                &cancel,
            );
        }
    })();
    match result {
        Ok(run) => {
            let outcome = match run.status {
                GraphStatus::Complete if run.checkpoint.successful() => Outcome::Ok,
                GraphStatus::Complete => Outcome::Failed,
                GraphStatus::Cancelled => Outcome::Cancelled,
                GraphStatus::Paused => Outcome::Refused,
                GraphStatus::Pending => Outcome::Timeout,
            };
            let continuation = format!(
                "provenant fabric graph resume --document {}",
                document.display()
            );
            let mut human = graph_human(&run);
            if matches!(run.status, GraphStatus::Pending | GraphStatus::Paused) {
                human.push_str(&format!("Continue: {continuation}\n"));
            } else if let Some(result) = run
                .checkpoint
                .nodes
                .iter()
                .rev()
                .filter_map(|node| {
                    node.outputs
                        .as_ref()
                        .and_then(|outputs| outputs.result.as_ref())
                })
                .next()
            {
                human.push_str(&result.excerpt);
                human.push('\n');
            }
            crate::emit(
                &provenant_records::CommandResult::new(
                    "run",
                    outcome,
                    serde_json::json!({"graph":run,"document":document,"continue":continuation}),
                ),
                globals,
                &human,
                false,
            )
        }
        Err(error) => crate::fail(
            "run",
            Outcome::Error,
            "preset_graph",
            &error.to_string(),
            globals,
        ),
    }
}
