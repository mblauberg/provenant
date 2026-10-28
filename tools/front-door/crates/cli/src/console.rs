//! Local, read-only composition of project and chair discovery owners.
use clap::Subcommand;
use provenant_core::{ProjectId, Roots};
use provenant_records::CanonicalV1;
use provenant_workflow::session::{HostSessionId, discover_sessions};
use serde::Serialize;
use serde_json::{Value, json};
use std::os::unix::ffi::OsStrExt;

#[derive(Debug, Subcommand)]
pub enum Console {
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
    #[error("catalogue exceeds the local scan limit")]
    Limit,
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

/// Compose existing owners without allocating, creating or repairing state.
pub fn read(roots: &Roots, selected: Option<ProjectId>, command: &Console) -> Result<Value, Error> {
    let projects = projects(roots, selected)?;
    let (session, limit, cursor) = match command {
        Console::Projects => {
            let result = json!({"projects":projects, "coverage":{"scope":"local"}});
            if serde_json::to_vec(&result)
                .map_err(|_| Error::Encode)?
                .len()
                > 900_000
            {
                return Err(Error::Limit);
            }
            return Ok(result);
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
    for project in &projects {
        for found in discover_sessions(&roots.state, project.project_id)? {
            scanned += 1;
            if scanned > 16_384 {
                return Err(Error::Limit);
            }
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
    }
    let snapshot = CanonicalV1::record_digest(
        "console-sessions",
        1,
        &(selected, session, limit, &sessions),
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
        "truncated":end < sessions.len(), "coverage":{"scope":"local"}}),
    )
}
