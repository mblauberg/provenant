//! Local, read-only composition of project and chair discovery owners.
use clap::Subcommand;
use provenant_core::{ProjectId, Roots};
use provenant_records::CanonicalV1;
use provenant_workflow::session::{HostSessionId, discover_sessions_bounded};
use serde::Serialize;
use serde_json::{Value, json};
use std::os::unix::ffi::OsStrExt;

mod overview;

#[derive(Debug, Subcommand)]
pub enum Console {
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
    Project(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Session(#[from] provenant_workflow::session::SessionError),
    #[error("invalid session ID")]
    InvalidSession,
    #[error("invalid or stale cursor; restart the catalogue without --cursor")]
    Cursor,
    #[error("cannot encode catalogue")]
    Encode,
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
    let projects = projects(roots, selected)?;
    let (session, limit, cursor) = match command {
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
