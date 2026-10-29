//! Local, read-only composition of project and chair discovery owners.
use clap::Subcommand;
use provenant_core::{ProjectId, Roots};
use provenant_records::CanonicalV1;
use provenant_workflow::session::{HostSessionId, discover_sessions_bounded};
use serde::Serialize;
use serde_json::{Value, json};
use std::os::unix::ffi::OsStrExt;

mod board;
mod overview;
pub mod peer;
mod tree;
pub mod tree_peer;

pub use overview::summary_comparison;

#[derive(Debug, Subcommand)]
pub enum Console {
    /// Read local owner change generations
    Changes {
        /// Previous generations as a JSON object
        #[arg(long)]
        since: Option<String>,
    },
    /// Read a bounded local tree of nested runs
    Tree {
        /// Maximum store depth, including the host store
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u64).range(1..=16))]
        max_depth: u64,
        /// Maximum attempts to include
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u64).range(1..=100))]
        max_nodes: u64,
        /// Include complete run and attempt records
        #[arg(long)]
        detail: bool,
        /// Include configured peers: all or a comma-separated list of labels
        #[arg(long, value_name = "all|LABEL,...")]
        hosts: Option<String>,
    },
    /// Read a local board snapshot without artifact bodies
    Board {
        /// Include retained terminal attempts
        #[arg(long)]
        all: bool,
        /// Maximum rows in a page
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..=100))]
        limit: u64,
        /// Continue the same filters and attempt high-water mark
        #[arg(long)]
        cursor: Option<String>,
        /// Include configured peers: all or a comma-separated list of labels
        #[arg(long, value_name = "all|LABEL,...")]
        hosts: Option<String>,
    },
    /// Summarise local attention and running work
    Overview,
    /// List registered local projects
    Projects,
    /// List explicitly registered local chair sessions
    Sessions {
        /// Select a host session ID
        session: Option<String>,
        /// Maximum rows in a page
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..=100))]
        limit: u64,
        /// Continue an unchanged catalogue snapshot
        #[arg(long)]
        cursor: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Run(#[from] provenant_run::RunError),
    #[error(
        "invalid or stale board cursor; restart console board with the same filters and without --cursor"
    )]
    BoardCursor,
    #[error(transparent)]
    Project(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Session(#[from] provenant_workflow::session::SessionError),
    #[error("invalid session ID")]
    InvalidSession,
    #[error("invalid or stale cursor; restart the catalogue without --cursor")]
    Cursor,
    #[error("cannot encode catalogue")]
    Encode,
    #[error("invalid generations; pass the generations object from a previous changes response")]
    InvalidGenerations,
    #[error(transparent)]
    Changes(#[from] std::io::Error),
}

#[derive(Serialize)]
struct Project {
    project_id: ProjectId,
    common_dir: Value,
}

fn projects(roots: &Roots, selected: Option<ProjectId>) -> Result<Vec<Project>, Error> {
    let mut projects = provenant_config::registered_projects(roots)?
        .into_iter()
        .filter(|p| selected.is_none_or(|id| p.project_id() == id))
        .map(|p| {
            let path = p.common_dir().as_os_str().as_bytes();
            let common_dir = match std::str::from_utf8(path) {
                Ok(text) => json!({"encoding":"utf8", "path":text}),
                Err(_) => json!({"encoding":"native_bytes", "path":provenant_core::hex(path)}),
            };
            Project {
                project_id: p.project_id(),
                common_dir,
            }
        })
        .collect::<Vec<_>>();
    projects.sort_by_key(|p| p.project_id.to_string());
    Ok(projects)
}

/// Compose existing owners without creating or repairing state.
pub fn read(roots: &Roots, selected: Option<ProjectId>, command: &Console) -> Result<Value, Error> {
    read_with_budget(roots, selected, command, 16_384)
}

fn read_with_budget(
    roots: &Roots,
    selected: Option<ProjectId>,
    command: &Console,
    budget: usize,
) -> Result<Value, Error> {
    if let Console::Changes { since } = command {
        let current = serde_json::to_value(provenant_os::changes::read(roots.state.as_path())?)
            .map_err(|_| Error::Encode)?;
        let previous: Option<Value> = since
            .as_ref()
            .map(|text| {
                if text.len() > 1024 {
                    return Err(Error::InvalidGenerations);
                }
                let value: Value =
                    serde_json::from_str(text).map_err(|_| Error::InvalidGenerations)?;
                let object = value.as_object().ok_or(Error::InvalidGenerations)?;
                let names = ["epoch", "inbox", "board", "chair", "quota", "configuration"];
                if object.len() != names.len()
                    || names.iter().any(|name| {
                        object
                            .get(*name)
                            .is_none_or(|v| !v.is_null() && v.as_u64().is_none())
                    })
                {
                    return Err(Error::InvalidGenerations);
                }
                Ok(value)
            })
            .transpose()?;
        let mut changed = serde_json::Map::new();
        for name in ["inbox", "board", "chair", "quota", "configuration"] {
            let now = current[name].as_u64();
            let before = previous.as_ref().and_then(|value| value[name].as_u64());
            let same_epoch = current["epoch"].as_u64().is_some()
                && current["epoch"] == *previous.as_ref().map_or(&Value::Null, |v| &v["epoch"]);
            let comparison = match (now, before) {
                (Some(now), Some(before)) if same_epoch && now >= before => json!(now > before),
                _ => Value::Null,
            };
            changed.insert(name.to_owned(), comparison);
        }
        return Ok(json!({"generations": current, "changed": changed}));
    }
    if let Console::Board {
        all, limit, cursor, ..
    } = command
    {
        return board::read(roots, selected, *all, *limit, cursor.as_deref());
    }
    if let Console::Tree {
        max_depth,
        max_nodes,
        detail,
        ..
    } = command
    {
        return tree::read(roots, selected, *max_depth, *max_nodes, *detail);
    }
    let projects = projects(roots, selected)?;
    let (session, limit, cursor) = match command {
        Console::Changes { .. } => unreachable!("changes handled above"),
        Console::Board { .. } => unreachable!("board handled above"),
        Console::Tree { .. } => unreachable!("tree handled above"),
        Console::Overview => return overview::read(roots, &projects),
        Console::Projects => {
            return Ok(json!({"projects":projects, "coverage":{"scope":"local"}}));
        }
        Console::Sessions {
            session,
            limit,
            cursor,
        } => (session, limit, cursor),
    };
    if let Some(id) = session {
        HostSessionId::parse(id).map_err(|_| Error::InvalidSession)?;
    }
    let mut sessions = Vec::new();
    let mut scanned = 0usize;
    let mut scan_truncated = false;
    for project in &projects {
        let page = discover_sessions_bounded(&roots.state, project.project_id, budget - scanned)?;
        scanned += page.examined;
        for found in page.sessions {
            if session
                .as_ref()
                .is_none_or(|id| id == found.record.host_session_id.as_str())
            {
                sessions.push(
                    json!({"record":found.record, "consistency":found.consistency,
                    "role":"chair", "evidence":"checkpoint_save", "summary":null,
                    "summary_status":"unavailable"}),
                );
            }
        }
        if page.truncated {
            scan_truncated = true;
            break;
        }
    }
    let snapshot = CanonicalV1::record_digest(
        "console-sessions",
        1,
        &(selected, session, limit, scan_truncated, &sessions),
    )
    .map_err(|_| Error::Encode)?
    .to_string();
    let offset = if let Some(cursor) = cursor {
        let (digest, offset) = cursor.rsplit_once(':').ok_or(Error::Cursor)?;
        if digest != snapshot {
            return Err(Error::Cursor);
        }
        let offset: usize = offset.parse().map_err(|_| Error::Cursor)?;
        if offset == 0 || offset >= sessions.len() {
            return Err(Error::Cursor);
        }
        offset
    } else {
        0
    };
    let end = offset.saturating_add(*limit as usize).min(sessions.len());
    let next_cursor = (end < sessions.len()).then(|| format!("{snapshot}:{end}"));
    Ok(
        json!({"snapshot":snapshot, "sessions":&sessions[offset..end], "next_cursor":next_cursor,
        "truncated":scan_truncated || end < sessions.len(), "scan_truncated":scan_truncated,
        "coverage":{"scope":"local"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::RootInputs;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn changes_reports_unknown_and_comparison() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let home = base.join("home");
        let state = base.join("state");
        let checkout = base.join("checkout");
        for path in [&home, &state, &checkout] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut inputs = RootInputs::new(home, checkout);
        inputs.state_dir = Some(state.clone());
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        let first = read(&roots, None, &Console::Changes { since: None }).unwrap();
        assert!(first["generations"]["inbox"].is_null());
        provenant_os::changes::bump(&state, provenant_core::changes::Scope::Inbox).unwrap();
        let baseline = read(&roots, None, &Console::Changes { since: None }).unwrap();
        let since = baseline["generations"].to_string();
        let unchanged = read(
            &roots,
            None,
            &Console::Changes {
                since: Some(since.clone()),
            },
        )
        .unwrap();
        assert_eq!(unchanged["changed"]["inbox"], false);
        provenant_os::changes::bump(&state, provenant_core::changes::Scope::Inbox).unwrap();
        let changed = read(
            &roots,
            None,
            &Console::Changes {
                since: Some(since.clone()),
            },
        )
        .unwrap();
        assert_eq!(changed["changed"]["inbox"], true);
        assert!(changed["changed"]["board"].is_null());
        fs::remove_file(state.join(provenant_core::changes::FILE_NAME)).unwrap();
        let missing = read(
            &roots,
            None,
            &Console::Changes {
                since: Some(since.clone()),
            },
        )
        .unwrap();
        assert!(missing["generations"]["inbox"].is_null());
        assert!(missing["changed"]["inbox"].is_null());
        provenant_os::changes::bump(&state, provenant_core::changes::Scope::Inbox).unwrap();
        let reset = read(&roots, None, &Console::Changes { since: Some(since) }).unwrap();
        assert!(reset["changed"]["inbox"].is_null());
        assert!(matches!(
            read(
                &roots,
                None,
                &Console::Changes {
                    since: Some("{}".into())
                }
            ),
            Err(Error::InvalidGenerations)
        ));
    }

    #[test]
    fn scan_budget_counts_invalid_entries_across_projects() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let home = base.join("home");
        let state = base.join("state");
        let checkout = base.join("checkout");
        for path in [&home, &state, &checkout] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut inputs = RootInputs::new(home, checkout);
        inputs.state_dir = Some(state.clone());
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        for name in ["one", "two"] {
            let common = base.join(name).join(".git");
            fs::create_dir_all(&common).unwrap();
            let binding = provenant_config::bind_project(
                &roots,
                provenant_config::ProjectLocation::new(common).unwrap(),
            )
            .unwrap();
            let sessions = state
                .join("workflow/projects")
                .join(binding.project_id().to_string())
                .join("sessions");
            fs::create_dir_all(&sessions).unwrap();
            for name in ["!", "?"] {
                fs::write(sessions.join(name), b"").unwrap();
            }
        }
        assert_eq!(
            provenant_config::registered_projects(&roots).unwrap().len(),
            2
        );
        let command = Console::Sessions {
            session: None,
            limit: 50,
            cursor: None,
        };
        let truncated = read_with_budget(&roots, None, &command, 3).unwrap();
        assert_eq!(truncated["sessions"], json!([]));
        assert_eq!(truncated["scan_truncated"], true);
        assert_eq!(truncated["truncated"], true);
        assert!(truncated["next_cursor"].is_null());
        let complete = read_with_budget(&roots, None, &command, 5).unwrap();
        assert_eq!(complete["scan_truncated"], false);
        assert_eq!(complete["truncated"], false);
        assert_ne!(truncated["snapshot"], complete["snapshot"]);
    }
}
