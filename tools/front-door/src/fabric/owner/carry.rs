//! Carry the submitting checkout into a newly created writer under custody.
use super::*;
use provenant_records::lane_contract::CarryMode;
use provenant_run::worktree_carry::{self, CARRY_TIMEOUT, CarrySnapshot};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, path::Path};

const RECEIPT: &OsStr = OsStr::new("provenant-carried-edits.json");

#[derive(Serialize, Deserialize)]
struct Receipt {
    graph: String,
    node: String,
    base: String,
    head: Option<String>,
    paths: Vec<String>,
}

fn failure(path: &Path, detail: impl ToString) -> provenant_confinement::worktrees::WorktreeError {
    provenant_confinement::worktrees::WorktreeError::UnknownOccupancy {
        path: path.to_owned(),
        detail: format!("carrying chair edits: {}", detail.to_string()),
    }
}

pub(super) fn mode(
    owner: &BoundGraphOwner<'_>,
    override_mode: Option<CarryMode>,
) -> Result<CarryMode, FacadeError> {
    if let Some(mode) = override_mode {
        return Ok(mode);
    }
    let config = provenant_cli::config_reads::read_for(&owner.factory.context.roots, "run")
        .map_err(|error| FacadeError::io(format!("read carry setting: {error}")))?;
    let mut value = config
        .knobs
        .get("lanes.carry_uncommitted")
        .and_then(|knob| knob.value.as_str())
        .unwrap_or("tracked")
        .to_owned();
    let project = &owner.factory.context.project_root;
    let mut files = Vec::new();
    if let Some(dir) =
        provenant_config::host_project_dir(owner.factory.context.roots.state.as_path(), project)
    {
        files.push(dir.join("config.toml"));
    }
    files.push(project.join(".provenant/config.toml"));
    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(FacadeError::io(format!("read {}: {error}", path.display()))),
        };
        let doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| FacadeError::io(format!("read {}: {error}", path.display())))?;
        if let Some(knobs) = doc.get("knobs").and_then(toml_edit::Item::as_table_like)
            && let Some(item) = knobs.get("lanes.carry_uncommitted").or_else(|| {
                knobs
                    .get("lanes")?
                    .as_table_like()?
                    .get("carry_uncommitted")
            })
        {
            value = item
                .as_str()
                .ok_or_else(|| FacadeError::io("invalid carry setting"))?
                .to_owned();
        }
    }
    match value.as_str() {
        "off" => Ok(CarryMode::Off),
        "tracked" => Ok(CarryMode::Tracked),
        "all" => Ok(CarryMode::All),
        _ => Err(FacadeError::io("invalid lanes.carry_uncommitted setting")),
    }
}

fn with_target<T>(
    owner: &BoundGraphOwner<'_>,
    target: &Path,
    action: impl FnOnce(
        &provenant_confinement::worktrees::WorktreeContext<'_>,
        &mut provenant_confinement::VerifiedWorktree,
    ) -> Result<T, provenant_confinement::worktrees::WorktreeError>,
) -> Result<T, FacadeError> {
    use provenant_confinement::worktrees;
    worktrees::with_context_using_facts(
        &owner.factory.context.roots,
        &owner.factory.home,
        &owner.factory.cwd,
        &owner.factory.clock,
        owner.cancel,
        &owner.repo_facts,
        |context| {
            let mut tree = provenant_confinement::verify_worktree(context.repo, target)?;
            let _guard = worktrees::cache_writer_guard(context.project_state, tree.path())
                .map_err(worktrees::custody_refusal)?;
            worktrees::check_writer_terminal(tree.repo())?;
            action(context, &mut tree)
        },
    )
    .map_err(|error| run_error(error.into()))
}

pub(super) fn recover(
    owner: &BoundGraphOwner<'_>,
    target: &Path,
    request: &provenant_fabric_runtime::graph::WriterCarryRequest<'_>,
    base: &str,
) -> Result<Option<(String, Vec<String>)>, FacadeError> {
    if !target.exists() {
        return Ok(None);
    }
    with_target(owner, target, |_context, tree| {
        let bytes = match tree.repo().private_dir().read_stable(RECEIPT, 1024 * 1024) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let receipt: Receipt =
            serde_json::from_slice(&bytes).map_err(|error| failure(target, error))?;
        if receipt.graph != request.graph || receipt.node != request.node || receipt.base != base {
            return Err(failure(target, "carry receipt belongs to another launch"));
        }
        let head = receipt
            .head
            .ok_or_else(|| failure(target, "an interrupted carry requires inspection"))?;
        let env = provenant_run::build::prepare::lane_environment(&owner.factory.home);
        let observed = worktree_carry::head(
            target,
            &env,
            &owner.factory.executor,
            owner.cancel,
            CARRY_TIMEOUT,
        )
        .map_err(|error| failure(target, error))?;
        if observed.to_string() != head {
            return Err(failure(target, "carried commit moved"));
        }
        worktree_carry::ensure_clean(
            target,
            &env,
            &owner.factory.executor,
            owner.cancel,
            CARRY_TIMEOUT,
        )
        .map_err(|error| failure(target, error))?;
        Ok(Some((head, receipt.paths)))
    })
}

pub(super) fn capture(
    owner: &BoundGraphOwner<'_>,
    request: &provenant_fabric_runtime::graph::WriterCarryRequest<'_>,
    base: &str,
) -> Result<Option<CarrySnapshot>, FacadeError> {
    let mode = mode(owner, request.mode)?;
    if mode == CarryMode::Off {
        return Ok(None);
    }
    let source = provenant_run::resolve_reader_source(
        owner.factory.context.roots.state.as_path(),
        &owner.factory.cwd,
        &owner.factory.cwd,
    )
    .map_err(run_error)?;
    let env = provenant_run::build::prepare::lane_environment(&owner.factory.home);
    let observed = worktree_carry::head(
        &source,
        &env,
        &owner.factory.executor,
        owner.cancel,
        CARRY_TIMEOUT,
    )
    .map_err(|error| FacadeError::io(format!("read chair HEAD: {error}")))?;
    if observed.to_string() != base {
        return Ok(None);
    }
    let snapshot = worktree_carry::snapshot(
        &source,
        mode,
        &env,
        &owner.factory.executor,
        owner.cancel,
        CARRY_TIMEOUT,
    )
    .map_err(|error| FacadeError::io(format!("snapshot chair edits: {error}")))?;
    if worktree_carry::snapshot_base(
        &source,
        &snapshot,
        &env,
        &owner.factory.executor,
        owner.cancel,
        CARRY_TIMEOUT,
    )
    .map_err(|error| FacadeError::io(format!("inspect carry snapshot: {error}")))?
    .is_some_and(|parent| parent != observed)
    {
        return Err(FacadeError::io(
            "chair HEAD changed while snapshotting edits",
        ));
    }
    let after = worktree_carry::head(
        &source,
        &env,
        &owner.factory.executor,
        owner.cancel,
        CARRY_TIMEOUT,
    )
    .map_err(|error| FacadeError::io(format!("recheck chair HEAD: {error}")))?;
    if after != observed {
        return Err(FacadeError::io("chair HEAD changed during carry snapshot"));
    }
    Ok(Some(snapshot))
}

pub(super) fn apply(
    owner: &BoundGraphOwner<'_>,
    target: &Path,
    request: &provenant_fabric_runtime::graph::WriterCarryRequest<'_>,
    base: &str,
    snapshot: &CarrySnapshot,
) -> Result<Vec<String>, FacadeError> {
    use provenant_os::fs::PublishMode;
    with_target(owner, target, |_context, tree| {
        let env = provenant_run::build::prepare::lane_environment(&owner.factory.home);
        worktree_carry::ensure_clean(
            target,
            &env,
            &owner.factory.executor,
            owner.cancel,
            CARRY_TIMEOUT,
        )
        .map_err(|error| failure(target, error))?;
        let current = worktree_carry::head(
            target,
            &env,
            &owner.factory.executor,
            owner.cancel,
            CARRY_TIMEOUT,
        )
        .map_err(|error| failure(target, error))?;
        if current.to_string() != base {
            return Err(failure(target, "writer HEAD changed before carry"));
        }
        let mut receipt = Receipt {
            graph: request.graph.into(),
            node: request.node.into(),
            base: base.into(),
            head: None,
            paths: snapshot.paths.clone(),
        };
        let bytes = serde_json::to_vec(&receipt).map_err(|error| failure(target, error))?;
        if bytes.len() > 1024 * 1024 - 128 {
            return Err(failure(
                target,
                "carried path list exceeds the recovery receipt limit",
            ));
        }
        tree.repo()
            .private_dir()
            .atomic_write(RECEIPT, ".carry-", &bytes, PublishMode::NoReplace)?
            .require_durable()
            .map_err(|error| failure(target, error))?;
        let head = if snapshot.is_empty() {
            current
        } else {
            let head = worktree_carry::apply(
                target,
                snapshot,
                &env,
                &owner.factory.executor,
                owner.cancel,
                CARRY_TIMEOUT,
            )
            .map_err(|error| failure(target, error))?
            .ok_or_else(|| failure(target, "carry did not create its commit"))?;
            let parent = worktree_carry::commit_parent(
                target,
                head,
                &env,
                &owner.factory.executor,
                owner.cancel,
                CARRY_TIMEOUT,
            )
            .map_err(|error| failure(target, error))?;
            if parent.to_string() != base {
                return Err(failure(
                    target,
                    "writer HEAD changed while committing the carry",
                ));
            }
            worktree_carry::ensure_clean(
                target,
                &env,
                &owner.factory.executor,
                owner.cancel,
                CARRY_TIMEOUT,
            )
            .map_err(|error| failure(target, error))?;
            head
        };
        receipt.head = Some(head.to_string());
        let bytes = serde_json::to_vec(&receipt).map_err(|error| failure(target, error))?;
        tree.repo()
            .private_dir()
            .atomic_write(RECEIPT, ".carry-", &bytes, PublishMode::Replace)?
            .require_durable()
            .map_err(|error| failure(target, error))?;
        Ok(receipt.paths)
    })
}
