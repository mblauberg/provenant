use super::*;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ScratchPreparationError(
    pub(crate) provenant_confinement::worktrees::WorktreeError,
);

impl ScratchPreparationError {
    fn is_transient(&self) -> bool {
        use provenant_confinement::worktrees::WorktreeError;
        use provenant_os::fs::FsError;
        if self.0.admission_cause()
            == Some(provenant_confinement::worktrees::AdmissionCause::RegistryLockTimeout)
        {
            return true;
        }
        matches!(
            &self.0,
            WorktreeError::Filesystem(FsError::LockTimeout { .. })
                | WorktreeError::Utility(provenant_confinement::utility::UtilityError::Filesystem(
                    FsError::LockTimeout { .. }
                ))
                | WorktreeError::Utility(provenant_confinement::utility::UtilityError::TimedOut)
        )
    }
}

#[derive(Default)]
pub(crate) struct PreparedScratch(Option<std::path::PathBuf>);

impl PreparedScratch {
    pub(crate) fn get_or_prepare(
        &mut self,
        prepare: impl FnOnce() -> Result<std::path::PathBuf, ScratchPreparationError>,
    ) -> Result<&std::path::Path, ScratchPreparationError> {
        if self.0.is_none() {
            self.0 = Some(prepare()?);
        }
        self.0.as_deref().ok_or(ScratchPreparationError(
            provenant_confinement::worktrees::WorktreeError::InvalidRequest,
        ))
    }
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
    let graphs = scratch.join("graphs");
    let dir = Directory::open_anchor(&graphs)?;
    let uid = provenant_os::fs::effective_uid();
    let mut directory_metadata = dir.metadata()?;
    if directory_metadata.uid == uid
        && directory_metadata.mode & 0o077 != 0
        && dir.make_private().is_ok()
    {
        directory_metadata = dir.metadata()?;
    }
    if let Some(refusal) = private_graph_directory_refusal(
        &graphs,
        directory_metadata.uid,
        directory_metadata.mode,
        uid,
    ) {
        return Err(refusal.into());
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
                || dir.read_stable(
                    OsStr::new(&filename),
                    provenant_fabric_runtime::config::GraphLimits::document_bytes_bound()
                        .maximum_u64(),
                )? != bytes
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

fn retry_collected_document<T, E: std::error::Error + 'static>(
    path: &std::path::Path,
    mut run: impl FnMut() -> Result<T, E>,
    republish: impl FnOnce() -> Result<(), Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    match run() {
        Err(_) if !path.exists() => {
            republish()?;
            Ok(run()?)
        }
        result => Ok(result?),
    }
}

/// `None` when the graph directory is owned by `current` and private (mode 0700
/// after any repair). A foreign owner is told to remove or take over the
/// directory; an owned one is told the observed mode and `chmod 700`.
fn private_graph_directory_refusal(
    path: &std::path::Path,
    owner: u32,
    mode: u32,
    current: u32,
) -> Option<String> {
    if owner != current {
        return Some(format!(
            "preset graph directory {} is owned by uid {owner}, not the current user; remove it or chown it to yourself",
            path.display()
        ));
    }
    (mode & 0o077 != 0).then(|| {
        format!(
            "preset graph directory {} has mode {:04o} and is not private to the current user; run `chmod 700 {}`",
            path.display(),
            mode & 0o7777,
            path.display()
        )
    })
}

/// Preset waiting advances the retained immutable graph, including CI gates.
pub(crate) fn run_preset_graph(
    document: serde_json::Value,
    wait_seconds: u64,
    warnings: &[String],
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
    review: bool,
) -> u8 {
    use provenant_fabric_runtime::graph::GraphStatus;
    use provenant_records::Outcome;
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let factory = factory(globals, workers)?;
        if factory.context.in_lane {
            return Err(CoordinationError::LaneMutation.into());
        }
        let cancel = CancelToken::new();
        let document_model: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(document.clone())?;
        let graph_id = document_model.graph_id.clone();
        let waiting = WaitBudget::cli(factory.clock.mono_now(), wait_seconds)?;
        let mut prepared_scratch = PreparedScratch::default();
        let mut last_run = None;
        let mut admitted_graph_id = graph_is_pending(&factory, &graph_id).then_some(graph_id);
        let mut retained_document: Option<PathBuf> = None;
        let mut scratch_error = None;
        loop {
            if waiting.remaining(factory.clock.mono_now()).as_micros() == 0
                && (last_run.is_some() || scratch_error.is_some())
            {
                if let Some(error) = scratch_error {
                    let message =
                        scratch_prepare_error_message(&error, admitted_graph_id.as_deref(), true)
                            .ok_or_else(|| scratch_prepare_error(error))?;
                    return Err(message.into());
                }
                return Ok((
                    last_run.ok_or("preset wait budget expired before polling")?,
                    retained_document.ok_or("preset graph document was not retained")?,
                ));
            }
            let now = factory.clock.mono_now();
            let remaining = waiting.remaining(now);
            let budget = WaitBudget::cli(now, 55)?;
            let scratch_budget = WaitBudget::up_to(
                now,
                remaining.min(provenant_core::DurationMicros::from_micros(30_000_000)),
            );
            let scratch = match prepared_scratch
                .get_or_prepare(|| factory.prepare_scratch(&cancel, scratch_budget))
            {
                Ok(scratch) => scratch,
                Err(error) => {
                    let still_pending = admitted_graph_id
                        .as_deref()
                        .is_some_and(|id| graph_is_pending(&factory, id));
                    if retry_scratch_poll(&error, still_pending) {
                        scratch_error = Some(error);
                        provenant_core::Pause::wait(
                            &factory.backoff,
                            provenant_core::DurationMicros::from_micros(
                                250_000
                                    .min(waiting.remaining(factory.clock.mono_now()).as_micros()),
                            ),
                            &cancel,
                        );
                        continue;
                    }
                    return Err(scratch_prepare_error(error).into());
                }
            };
            // Poll at least once once scratch is ready, as before #2512.
            let document_path = match &retained_document {
                Some(path) => path.clone(),
                None => {
                    let path = publish_preset_graph(scratch, &document)?;
                    retained_document = Some(path.clone());
                    path
                }
            };
            let run_document = || {
                factory.graph_with_scratch(
                    GraphInput::Run {
                        extra: Default::default(),
                        document: document_path.clone(),
                    },
                    budget,
                    &cancel,
                    Some(scratch),
                )
            };
            // Collection may retire an old graph after publication but before
            // the graph reads the content-addressed document.
            let run = retry_collected_document(&document_path, run_document, || {
                publish_preset_graph(scratch, &document).map(|_| ())
            })?;
            // Collection may also have removed the file after it was opened.
            // A pending checkpoint now prevents another collection of this graph.
            if run.status == GraphStatus::Pending && !document_path.exists() {
                publish_preset_graph(scratch, &document)?;
            }
            scratch_error = None;
            admitted_graph_id = Some(run.checkpoint.graph_id.clone());
            last_run = Some(run.clone());
            if run.status != GraphStatus::Pending
                || waiting.remaining(factory.clock.mono_now()).as_micros() == 0
            {
                return Ok((run, document_path));
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
        Ok((run, document)) => {
            let graph_id = run.checkpoint.graph_id.clone();
            let mut status_command = String::from("provenant");
            if let Some(state_dir) = &globals.state_dir {
                status_command.push_str(&format!(" --state-dir {}", shell_path(state_dir)));
            }
            if let Some(project) = &globals.project {
                status_command.push_str(&format!(" --project {}", shell_path(project)));
            }
            status_command.push_str(&format!(" fabric graph status {graph_id}"));
            let verdict = if review {
                document_model_verdict(&document, &run.checkpoint)
            } else {
                None
            };
            let outcome = match run.status {
                GraphStatus::Complete
                    if run.outcome
                        != Some(provenant_fabric_runtime::graph::GraphOutcome::Failed)
                        && (!review
                            || verdict
                                == Some(provenant_records::workflow::ReviewVerdict::Approve)) =>
                {
                    Outcome::Ok
                }
                GraphStatus::Complete => Outcome::Failed,
                GraphStatus::Cancelled => Outcome::Cancelled,
                GraphStatus::Paused => Outcome::Refused,
                GraphStatus::Pending => Outcome::Waiting,
            };
            let continuation = format!(
                "provenant fabric graph resume --document {}",
                document.display()
            );
            let mut human = if run.status == GraphStatus::Pending {
                graph_waiting_human(&run, globals)
            } else {
                graph_human(&run)
            };
            human.push_str(&format!("Graph ID: {graph_id}\nStatus: {status_command}\n"));
            if run.status == GraphStatus::Paused {
                human.push_str(&format!("Continue: {continuation}\n"));
            } else if run.status != GraphStatus::Pending
                && let Some(result) = run
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
            let reviewers = if review {
                match review_rows(globals, workers, &run.checkpoint) {
                    Ok(rows) => Some(rows),
                    Err(error) => {
                        return crate::fail(
                            "review",
                            Outcome::Error,
                            "review_rows",
                            &error.to_string(),
                            globals,
                        );
                    }
                }
            } else {
                None
            };
            let mut data = serde_json::json!({"graph":run,"graph_id":graph_id,"status_command":status_command,"document":document,"continue":continuation});
            if let Some(rows) = reviewers {
                data["reviewers"] = serde_json::json!(rows);
            }
            if !warnings.is_empty() {
                data["warnings"] = serde_json::json!(warnings);
            }
            human.push_str(&provenant_cli::run::warnings_text(&data));
            let code = crate::emit(
                &provenant_records::CommandResult::new(
                    if review { "review" } else { "run" },
                    outcome,
                    data,
                ),
                globals,
                &human,
                false,
            );
            if review && run.status == GraphStatus::Pending {
                124
            } else {
                code
            }
        }
        Err(error) => crate::fail_with_repair(
            "run",
            Outcome::Error,
            "preset_graph",
            &error.to_string(),
            error
                .downcast_ref::<provenant_cli::fabric::EdgeError>()
                .and_then(|error| error.repair())
                .as_deref(),
            globals,
        ),
    }
}

/// Project the lens members of the retained graph in both waiting and final views.
fn review_rows(
    globals: &provenant_cli::Globals,
    workers: &'static dyn WorkerObserver,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let factory = factory(globals, workers)?;
    let cancel = CancelToken::new();
    let members = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "lenses")
        .map(|node| node.children.as_slice())
        .unwrap_or_default();
    members
        .iter()
        .filter_map(|id| checkpoint.nodes.iter().find(|node| &node.id == id))
        .map(|node| {
            let attempt = node.attempts.last();
            let (run_id, applied_route) = if let Some(attempt) = attempt {
                let owner = BinaryRunOwner {
                    factory: &factory,
                    allow_remote: false,
                    actor: Some(attempt.launch_actor.parse()?),
                    chair: None,
                };
                let store = owner.store(&cancel)?;
                let call = owner.call(&cancel);
                let operation = graph_operation("graph-attempt-operation", &attempt.key)?;
                let operation_run = store
                    .operation(&operation, &call)
                    .map_err(run_error)?
                    .map(|op| op.run_id);
                if let Some(reference) = attempt.attempt_ref.as_deref() {
                    let (host, id) = reference
                        .split_once('/')
                        .ok_or("invalid graph attempt reference")?;
                    let target = AttemptTarget {
                        extra: Default::default(),
                        host: Some(host.parse()?),
                        attempt: id.parse()?,
                    };
                    let budget = WaitBudget::cli(factory.clock.mono_now(), 55)?;
                    let record = owner.snapshot(target, budget, &cancel)?;
                    (Some(record.run_id), record.applied_route)
                } else {
                    (operation_run, None)
                }
            } else {
                (None, None)
            };
            Ok(serde_json::json!({
                "node_id": node.id,
                "lens": node.id.strip_prefix("lenses--lens-"),
                "run_id": run_id,
                "state": node.state,
                "attempt_ref": attempt.and_then(|a| a.attempt_ref.as_deref()),
                "substitution": review_substitution(node),
                "separation": node.separation,
                "verdict": node.outputs.as_ref().and_then(review_verdict),
                "applied_route": applied_route,
                "result": node.outputs.as_ref().and_then(|o| o.result.as_ref()),
            }))
        })
        .collect()
}

fn review_substitution(node: &provenant_fabric_runtime::graph::NodeProgress) -> bool {
    node.attempts.iter().any(|a| a.review_substitution)
        || node.separation.as_ref().is_some_and(|separation| {
            separation.outcome == provenant_fabric_runtime::graph::SeparationOutcome::Unavailable
        })
}

fn document_model_verdict(
    document: &std::path::Path,
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
) -> Option<provenant_records::workflow::ReviewVerdict> {
    let bytes = std::fs::read(document).ok()?;
    let document: provenant_fabric_runtime::graph::GraphDocument =
        serde_json::from_slice(&bytes).ok()?;
    let last = document
        .nodes
        .iter()
        .find(|node| {
            node.composite
                .as_ref()
                .is_some_and(|group| group.reduce_verdict)
        })
        .or_else(|| document.nodes.last())?;
    let outputs = checkpoint
        .nodes
        .iter()
        .find(|node| node.id == last.id)?
        .outputs
        .as_ref()?;
    review_verdict(outputs)
}

fn review_verdict(
    outputs: &provenant_fabric_runtime::graph::NodeOutputs,
) -> Option<provenant_records::workflow::ReviewVerdict> {
    if outputs.verdict_error.is_some() {
        return None;
    }
    outputs
        .verdict
        .as_deref()
        .and_then(provenant_records::workflow::ReviewVerdict::from_token)
}

fn graph_is_pending(factory: &Factory, graph_id: &str) -> bool {
    use provenant_fabric_runtime::graph::{FileJournal, GraphStatus};
    FileJournal::read_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph_id,
    )
    .ok()
    .flatten()
    .is_some_and(|checkpoint| graph_snapshot(checkpoint).status == GraphStatus::Pending)
}

pub(crate) fn retry_scratch_poll(error: &ScratchPreparationError, graph_admitted: bool) -> bool {
    graph_admitted && error.is_transient()
}

pub(crate) fn scratch_prepare_error_message(
    error: &ScratchPreparationError,
    graph_id: Option<&str>,
    exhausted: bool,
) -> Option<String> {
    if !error.is_transient() {
        return None;
    }
    match (graph_id, exhausted) {
        (Some(graph_id), true) => Some(format!(
            "graph {graph_id}: {}",
            scratch_prepare_error(error)
        )),
        _ => None,
    }
}

pub(super) fn shell_path(path: &std::path::Path) -> String {
    let value = path.to_string_lossy();
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_./-".contains(character))
    {
        value.into_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
#[allow(dead_code, unused_imports)] // The worktree driver includes this module without libtest functions.
mod tests {
    use super::{private_graph_directory_refusal, retry_collected_document};
    use std::path::Path;

    #[test]
    fn live_add_journal_lock_wait_spike() -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{CancelToken, Clock, ProjectId, RootInputs, StateRoot, WaitBudget};
        use provenant_fabric_runtime::graph::FileJournal;
        use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };

        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base);
        let root = StateRoot::resolve(&inputs, &SystemPathResolver)?;
        let project = ProjectId::from_bytes([1; 16]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        let held = FileJournal::open(&root, project, "live-add-spike", budget, &cancel, &clock)?;
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let clock = SystemClock::new();
                let cancel = CancelToken::new();
                let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
                started_tx.send(()).unwrap();
                let start = Instant::now();
                let journal =
                    FileJournal::open(&root, project, "live-add-spike", budget, &cancel, &clock);
                finished_tx
                    .send((start.elapsed(), journal.is_ok()))
                    .unwrap();
            });
            started_rx.recv().unwrap();
            assert!(
                finished_rx
                    .recv_timeout(Duration::from_millis(150))
                    .is_err()
            );
            drop(held);
            let (elapsed, opened) = finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(opened);
            eprintln!(
                "contended graph journal open waited {} ms",
                elapsed.as_millis()
            );
        });
        Ok(())
    }

    #[test]
    fn review_row_marks_unavailable_family_as_substitution() {
        let mut node: provenant_fabric_runtime::graph::NodeProgress = serde_json::from_value(serde_json::json!({
            "id":"lenses--lens-tests", "state":"succeeded", "worktree":null, "attempts":[],
            "separation":{"strength":"prefer", "excluded":["openai"], "applied":"openai", "outcome":"unavailable"}
        })).unwrap();
        assert!(super::review_substitution(&node));
        node.separation.as_mut().unwrap().outcome =
            provenant_fabric_runtime::graph::SeparationOutcome::Separated;
        assert!(!super::review_substitution(&node));
    }

    #[test]
    fn review_exit_evidence_requires_a_valid_approval() {
        use provenant_records::workflow::ReviewVerdict;
        let outputs = |verdict: Option<&str>, error: Option<&str>| {
            serde_json::from_value(serde_json::json!({
                "schema_version":2, "result":null, "verdict":verdict, "verdict_error":error,
                "findings":[], "branch":null, "commit":null, "warnings":[]
            }))
            .unwrap()
        };
        assert_eq!(
            super::review_verdict(&outputs(Some("VERDICT: PASS"), None)),
            Some(ReviewVerdict::Approve)
        );
        assert_eq!(
            super::review_verdict(&outputs(Some("VERDICT: FAIL"), None)),
            Some(ReviewVerdict::RequestChanges)
        );
        assert_eq!(super::review_verdict(&outputs(None, None)), None);
        assert_eq!(
            super::review_verdict(&outputs(Some("VERDICT: PASS"), Some("invalid"))),
            None
        );
    }

    #[test]
    fn collected_preset_document_is_republished_before_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("graph.json");
        std::fs::write(&path, b"document")?;
        let mut calls = 0;
        let result = retry_collected_document(
            &path,
            || {
                calls += 1;
                if calls == 1 {
                    std::fs::remove_file(&path)?;
                    return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
                }
                std::fs::read(&path)
            },
            || {
                std::fs::write(&path, b"document")?;
                Ok(())
            },
        )?;
        assert_eq!(calls, 2);
        assert_eq!(result, b"document");
        Ok(())
    }

    #[test]
    fn registry_lock_timeout_retries_only_after_admission() {
        use super::{ScratchPreparationError, retry_scratch_poll};
        use provenant_confinement::worktrees::WorktreeError;
        let error = ScratchPreparationError(WorktreeError::RegistryLockTimeout);
        assert!(retry_scratch_poll(&error, true));
        assert!(!retry_scratch_poll(&error, false));
    }

    #[test]
    fn refusal_names_the_path_mode_and_repair() {
        let path = Path::new("/repo/.provenant/scratch/graphs");
        assert_eq!(
            private_graph_directory_refusal(path, 501, 0o40700, 501),
            None
        );
        let loose = private_graph_directory_refusal(path, 501, 0o40755, 501)
            .expect("group and other bits are refused when repair fails");
        assert!(loose.contains("/repo/.provenant/scratch/graphs"), "{loose}");
        assert!(loose.contains("0755"), "{loose}");
        assert!(
            loose.contains("chmod 700 /repo/.provenant/scratch/graphs"),
            "{loose}"
        );
        let foreign = private_graph_directory_refusal(path, 0, 0o40700, 501)
            .expect("another owner is refused");
        assert!(
            foreign.contains("/repo/.provenant/scratch/graphs"),
            "{foreign}"
        );
        assert!(foreign.contains("owned by uid 0"), "{foreign}");
        assert!(foreign.contains("remove it or chown"), "{foreign}");
        assert!(!foreign.contains("chmod"), "{foreign}");
    }
}
