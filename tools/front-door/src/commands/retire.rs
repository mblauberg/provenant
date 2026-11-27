//! `retire` — archive a graph or fence a project out of scheduling.
//!
//! A graph is copied under the graph lock into
//! `<state>/fabric/graphs/<project>/retired/<id>-<ts>/` before its live record
//! is cleared, so an interrupted clear still leaves a recoverable copy. A
//! project keeps its registration and settings and gains a disposition fence;
//! `provenant adopt project:ID` removes that fence.

use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::Clock;
use provenant_core::WaitBudget;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, CommandResult, Outcome};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub static DOMAIN: Domain = Domain {
    name: "retire",
    verbs: &[Verb {
        path: &["retire"],
        stability: Stability::Evolving,
        run,
    }],
};

/// Upper bound on a single archived checkpoint. Larger records are left alone
/// rather than copied into an archive that cannot hold them.
const CHECKPOINT_LIMIT: u64 = 64 * 1024 * 1024;

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Retire(args) => {
            let action = || -> Result<(String, serde_json::Value), Box<dyn std::error::Error>> {
                let (kind, id) = args
                    .subject
                    .split_once(':')
                    .ok_or("expected graph:ID or project:ID")?;
                let resolved = crate::roots::resolve(
                    globals,
                    crate::roots::Workspace::Project,
                    crate::roots::Product::Required,
                )?;
                let context = provenant_cli::fabric::Context::open(
                    resolved.roots.clone(),
                    &resolved.home,
                    &resolved.project,
                )?;
                let project = context.identity.project;
                match kind {
                    "graph" => {
                        let archive = archive_graph(resolved.roots.state.as_path(), project, id)?;
                        Ok((
                            format!("retired graph {id} -> {}\n", archive.display()),
                            serde_json::json!({
                                "kind": "graph",
                                "id": id,
                                "archive": archive,
                            }),
                        ))
                    }
                    "project" => {
                        let target = match provenant_core::ProjectId::parse(id) {
                            Ok(project) => project,
                            Err(_) => provenant_config::project_id_for_path(Path::new(id))
                                .ok_or_else(|| {
                                    "project is neither a ProjectId nor a checkout path".to_owned()
                                })?,
                        };
                        let fenced = provenant_config::fence_project(&resolved.roots, target)?;
                        Ok((
                            format!("retired project {target} (fenced={fenced})\n"),
                            serde_json::json!({
                                "kind": "project",
                                "id": target.to_string(),
                                "fenced": fenced,
                            }),
                        ))
                    }
                    _ => Err("expected graph:ID or project:ID".into()),
                }
            };
            match action() {
                Ok((human, data)) => crate::emit(
                    &CommandResult::new("retire", Outcome::Ok, data),
                    globals,
                    &human,
                    true,
                ),
                Err(error) => crate::fail(
                    "retire",
                    Outcome::Error,
                    "RETIRE",
                    &error.to_string(),
                    "resolve the reported condition, then rerun the command",
                    globals,
                ),
            }
        }
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}

/// Copy a graph's exact stored bytes into a retired archive under the graph
/// lock, then clear the live record and its sidecars. Returns the archive
/// directory so the caller can name it in its report.
fn archive_graph(
    state: &Path,
    project: provenant_core::ProjectId,
    id: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use provenant_os::fs::{Directory, LockMode, PublishMode};
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.as_bytes().contains(&0) {
        return Err("invalid graph id".into());
    }
    let root = Directory::open_anchor(state)?;
    let fabric = root.open_verified_directory(OsStr::new("fabric"))?;
    let graphs = fabric.open_verified_directory(OsStr::new("graphs"))?;
    let project_name = OsString::from(project.to_string());
    let directory = graphs.open_verified_directory(&project_name)?;

    let clock = provenant_os::clock::SystemClock::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5)?;
    let cancel = provenant_core::CancelToken::new();
    let lock_name = OsString::from(format!("graph-{id}.lock"));
    let _lock = directory.lock(&lock_name, LockMode::Exclusive, budget, &cancel, &clock)?;

    let checkpoint = OsString::from(format!("graph-{id}.json"));
    let bytes = directory.read_bounded(&checkpoint, CHECKPOINT_LIMIT)?;
    let stamp = clock.try_unix_millis()?;
    let archive_name = format!("{id}-{stamp}");
    match directory.mkdir(OsStr::new("retired"), 0o700) {
        Ok(()) => {}
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let retired = directory.open_verified_directory(OsStr::new("retired"))?;
    match retired.mkdir(OsStr::new(&archive_name), 0o700) {
        Ok(()) => {}
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let archive = retired.open_verified_directory(OsStr::new(&archive_name))?;
    archive
        .atomic_write(&checkpoint, ".graph-retired.", &bytes, PublishMode::Replace)?
        .require_durable()?;
    archive.sync()?;
    retired.sync()?;

    // The copy is durable before the live record and its sidecars are removed.
    match directory.unlink(&checkpoint) {
        Ok(()) => {}
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    let unreadable = OsString::from(format!("graph-{id}.unreadable"));
    let _ = directory.unlink(&unreadable);
    let advance = OsString::from(format!("graph-{id}.advance-error"));
    let _ = directory.unlink(&advance);
    directory.sync()?;
    Ok(archive.current_path()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn retire_archives_before_clear() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        let project = provenant_core::ProjectId::from_bytes([7; 16]);
        let graph_dir = state.join("fabric/graphs").join(project.to_string());
        std::fs::create_dir_all(&graph_dir).unwrap();
        let fabric_dir = state.join("fabric");
        let graphs_dir = fabric_dir.join("graphs");
        for dir in [
            state.as_path(),
            fabric_dir.as_path(),
            graphs_dir.as_path(),
            graph_dir.as_path(),
        ] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let record = graph_dir.join("graph-g1.json");
        std::fs::write(&record, b"{\"checkpoint\":1}").unwrap();
        std::fs::write(graph_dir.join("graph-g1.unreadable"), b"quarantined").unwrap();

        let archive = archive_graph(&state, project, "g1").unwrap();
        let name = archive.to_string_lossy().into_owned();
        assert!(
            name.contains("retired"),
            "archive lives under retired/: {name}"
        );
        assert_eq!(
            std::fs::read(archive.join("graph-g1.json")).unwrap(),
            b"{\"checkpoint\":1}",
            "the exact stored bytes are copied before the live record is cleared"
        );
        assert!(!record.exists(), "the live checkpoint is cleared");
        assert!(
            !graph_dir.join("graph-g1.unreadable").exists(),
            "the quarantine marker is cleared with the record"
        );

        // Retiring again finds no live record to archive.
        assert!(archive_graph(&state, project, "g1").is_err());
    }
}
