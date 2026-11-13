//! Local, read-only composition of project and chair discovery owners.
use crate::metadata::CatalogueArg;
use clap::Subcommand;
use provenant_core::{ProjectId, Roots};
use provenant_records::CanonicalV1;
use provenant_workflow::session::{HostSessionId, discover_sessions_bounded};
use serde::Serialize;
use serde_json::{Value, json};
use std::os::unix::ffi::OsStrExt;

mod board;
mod detail;
mod detail_inbox;
mod overview;
pub mod peer;
pub mod peer_inbox;
mod search;
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
        #[arg(long, default_value_t = 8, catalog_u64_range = 1..=16)]
        max_depth: u64,
        /// Maximum attempts to include
        #[arg(long, default_value_t = 100, catalog_u64_range = 1..=100)]
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
        #[arg(long, default_value_t = 50, catalog_u64_range = 1..=100)]
        limit: u64,
        /// Continue the same filters and attempt high-water mark
        #[arg(long)]
        cursor: Option<String>,
        /// Include configured peers: all or a comma-separated list of labels
        #[arg(long, value_name = "all|LABEL,...")]
        hosts: Option<String>,
    },
    /// Read one attempt's bounded metadata and artifact inventory
    Detail {
        /// Attempt, as NAME[/NODE[#N]]
        #[arg(value_name = "TARGET")]
        target: crate::Target,
    },
    /// Search local prompt or result content; returns the first match per artifact.
    /// Each cursor page rescans the bounded local content; peers are not searched.
    Search {
        query: String,
        #[arg(long, value_parser = ["prompt", "result"])]
        part: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 50, catalog_u64_range = 1..=100)]
        limit: u64,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Summarise local attention and running work
    Overview,
    /// Open the native console app (macOS)
    Open,
    /// List registered local projects
    Projects,
    /// List explicitly registered local chair sessions
    Sessions {
        /// Select a host session ID
        session: Option<String>,
        /// Maximum rows in a page
        #[arg(long, default_value_t = 50, catalog_u64_range = 1..=100)]
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
    #[error(
        "invalid or stale search cursor; restart console search with the same query and filters and without --cursor"
    )]
    SearchCursor,
    #[error("cannot encode catalogue")]
    Encode,
    #[error("invalid generations; pass the generations object from a previous changes response")]
    InvalidGenerations,
    #[error(transparent)]
    Changes(#[from] std::io::Error),
    #[error("{0}")]
    Target(String),
}

/// The `HOST/ATTEMPT` reference of a target's attempt, read from its graph's
/// one checkpoint in the selected project, or else the first project that
/// holds a graph of that name.
fn detail_reference(
    roots: &Roots,
    selected: Option<ProjectId>,
    target: &crate::Target,
) -> Result<String, Error> {
    use crate::target::{ResolveError, Want, resolve};
    for project in projects(roots, selected)? {
        let resolved = resolve(target, Want::Attempt { settled: false }, |name| {
            provenant_fabric_runtime::graph::FileJournal::read_status_checkpoint(
                &roots.state,
                project.project_id,
                name,
            )
        });
        match resolved {
            Err(ResolveError::NotFound(_)) => continue,
            Err(ResolveError::Read(error)) => return Err(Error::Target(error.to_string())),
            Err(error) => return Err(Error::Target(error.to_string())),
            Ok(resolved) => {
                return resolved
                    .attempt_ref
                    .ok_or_else(|| Error::Target(format!("NOT_FOUND: {target} has not launched")));
            }
        }
    }
    Err(Error::Target(format!("NOT_FOUND: {target}")))
}

#[derive(Serialize)]
struct Project {
    project_id: ProjectId,
    common_dir: Value,
    label: Option<String>,
}

impl Project {
    fn new(project_id: ProjectId, path: &std::path::Path) -> Self {
        let bytes = path.as_os_str().as_bytes();
        let common_dir = match std::str::from_utf8(bytes) {
            Ok(text) => json!({"encoding":"utf8", "path":text}),
            Err(_) => json!({"encoding":"native_bytes", "path":provenant_core::hex(bytes)}),
        };
        Self {
            project_id,
            common_dir,
            label: path
                .parent()
                .and_then(std::path::Path::file_name)
                .and_then(|name| name.to_str())
                .map(str::to_owned),
        }
    }
}

fn projects(roots: &Roots, selected: Option<ProjectId>) -> Result<Vec<Project>, Error> {
    let mut projects = provenant_config::registered_projects(roots)?
        .into_iter()
        .filter(|p| selected.is_none_or(|id| p.project_id() == id))
        .map(|p| Project::new(p.project_id(), p.common_dir()))
        .collect::<Vec<_>>();
    projects.sort_by_key(|p| p.project_id.to_string());
    Ok(projects)
}

/// Compose existing owners without creating or repairing state.
/// One attempt's detail by its `[HOST/]ATTEMPT` reference: what a peer serves
/// once the caller's host resolved the target.
pub fn read_detail(roots: &Roots, reference: &str) -> Result<Value, Error> {
    detail::read(roots, reference)
}

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
        let mut current = serde_json::to_value(provenant_os::changes::read(roots.state.as_path())?)
            .map_err(|_| Error::Encode)?;
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let context = provenant_run::ExecutionContext {
            state_root: roots.state.clone(),
            project_id: ProjectId::from_bytes([0; 16]),
            actor_id: provenant_core::ActorId::from_bytes([0; 16]),
            read_prompts: false,
        };
        // Hints may be skipped; the run owner's event counter also covers
        // nonterminal transitions without reading attempts or artifact bodies.
        current["runs"] = provenant_run::RunStore::open_read_only(&context, &call)
            .ok()
            .flatten()
            .and_then(|store| store.board_generation(&call).ok())
            .map_or(Value::Null, |generation| json!(generation));
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
                // Accept a v1 baseline too: its absent runs value means reload.
                if !(object.len() == names.len()
                    || object.len() == names.len() + 1 && object.contains_key("runs"))
                    || object
                        .get("runs")
                        .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
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
        for name in ["inbox", "board", "runs", "chair", "quota", "configuration"] {
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
        return Ok(json!({"schema_version":2,"generations": current, "changed": changed}));
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
    if let Console::Detail { target } = command {
        return detail::read(roots, &detail_reference(roots, selected, target)?);
    }
    if let Console::Search {
        query,
        part,
        session,
        limit,
        cursor,
    } = command
    {
        return search::read(
            roots,
            query,
            part,
            session.as_deref(),
            *limit,
            cursor.as_deref(),
        );
    }
    let projects = projects(roots, selected)?;
    let (session, limit, cursor) = match command {
        Console::Changes { .. } => unreachable!("changes handled above"),
        Console::Board { .. } => unreachable!("board handled above"),
        Console::Tree { .. } => unreachable!("tree handled above"),
        Console::Open => unreachable!("open is handled at the process edge"),
        Console::Detail { .. } | Console::Search { .. } => unreachable!("read handled above"),
        Console::Overview => return overview::read(roots, &projects),
        Console::Projects => {
            return Ok(
                json!({"schema_version":2,"projects":projects, "coverage":{"scope":"local"}}),
            );
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
                    "role":"chair", "evidence":"checkpoint_save", "summary":found.record.summary,
                    "summary_status":if found.record.summary.is_some() { "available" } else { "unavailable" }}),
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
        json!({"schema_version":2,"snapshot":snapshot, "sessions":&sessions[offset..end], "next_cursor":next_cursor,
        "truncated":scan_truncated || end < sessions.len(), "scan_truncated":scan_truncated,
        "coverage":{"scope":"local"}}),
    )
}

#[cfg(test)]
fn write_fixture(
    name: &str,
    command: &str,
    data: Value,
    root: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(directory) = std::env::var_os("PROVENANT_CONSOLE_FIXTURES") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory)?;
        let envelope = provenant_records::CommandResult::new(
            command,
            provenant_records::Outcome::Ok,
            Some(data),
        );
        let text = serde_json::to_string_pretty(&envelope)?;
        let mut bytes = text
            .replace(root.to_str().ok_or("fixture root")?, "/fixture")
            .into_bytes();
        bytes.push(b'\n');
        std::fs::write(directory.join(format!("{name}.json")), bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::RootInputs;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn project_labels_preserve_native_path_encoding() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::ffi::OsStringExt;
        let id = ProjectId::from_bytes([1; 16]);
        let readable = serde_json::to_value(Project::new(
            id,
            std::path::Path::new("/fixture/console-project/.git"),
        ))?;
        assert_eq!(readable["label"], "console-project");
        assert_eq!(readable["common_dir"]["encoding"], "utf8");
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(
            b"/fixture/native-\xff/.git".to_vec(),
        ));
        let native = serde_json::to_value(Project::new(id, &path))?;
        assert!(native["label"].is_null());
        assert_eq!(native["common_dir"]["encoding"], "native_bytes");
        assert_eq!(
            native["common_dir"]["path"],
            provenant_core::hex(path.as_os_str().as_bytes())
        );
        Ok(())
    }

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
