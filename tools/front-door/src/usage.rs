//! Local demand counters. The command edge decides what to count and renders
//! the report; telemetry owns the log.
use crate::roots::{self, Product, Resolved, Workspace};
use provenant_cli::{Globals, Stats};
use provenant_core::{ProjectId, Roots};
use provenant_records::{CommandResult, Outcome};
use provenant_telemetry::usage::{self, EVENT_KINDS};

pub fn attribution_enabled(roots: &Roots) -> bool {
    provenant_cli::config_reads::read_for(roots, "knobs")
        .ok()
        .and_then(|config| config.knobs.get("usage.project_attribution").cloned())
        .and_then(|knob| knob.value.as_bool())
        .unwrap_or(false)
}

pub fn project_id(resolved: &Resolved) -> Option<ProjectId> {
    if !attribution_enabled(&resolved.roots) {
        return None;
    }
    use std::time::Duration;
    let git = provenant_os::git::git_executable()?;
    let runtime = provenant_confinement::UtilityRuntime::read_only(git).ok()?;
    let executor = provenant_os::exec::SystemExecutor::new(1).ok()?;
    let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(1)).ok()?;
    let location = runtime
        .project_location(
            &executor,
            &resolved.project,
            budget,
            &provenant_core::CancelToken::new(),
        )
        .ok()?
        .location;
    provenant_config::lookup_project(&resolved.roots, &location)
        .ok()?
        .map(|binding| binding.project_id())
}

/// Every installed CLI verb, so unused units report zero calls.
fn inventory() -> Vec<(String, String)> {
    provenant_cli::usage_verbs()
        .into_iter()
        .map(|verb| (EVENT_KINDS[0].into(), verb))
        .collect()
}

pub fn run(stats: &Stats, globals: &Globals) -> u8 {
    let Ok(resolved) = roots::resolve(globals, Workspace::Directory, Product::Optional) else {
        return crate::fail(
            "telemetry stats",
            Outcome::Error,
            "ROOT",
            "cannot resolve usage state root",
            globals,
        );
    };
    match usage::report(&resolved.roots, inventory(), stats.since, stats.until) {
        Ok(data) => {
            let text = data
                .units
                .iter()
                .map(|unit| {
                    let project = unit
                        .project_id
                        .map(|id| format!("  project={id}"))
                        .unwrap_or_default();
                    format!("{:>8}  {}  {}{project}\n", unit.calls, unit.kind, unit.unit)
                })
                .collect::<String>();
            crate::emit(
                &CommandResult::new("telemetry stats", Outcome::Ok, data),
                globals,
                &text,
                true,
            )
        }
        Err(error) => crate::fail(
            "telemetry stats",
            Outcome::Error,
            "USAGE",
            &error.to_string(),
            globals,
        ),
    }
}

/// Ingest native Claude sessions into the state root's token store. Registered
/// and other `~/.claude*` directories locate transcripts; identity comes only
/// from a chair session record and otherwise stays typed unknown.
pub fn ingest_native(
    roots: &Roots,
    home: &std::path::Path,
    provider: Option<&str>,
    since: Option<jiff::Timestamp>,
    deadline: Option<std::time::Instant>,
) -> Result<provenant_telemetry::ingest::Report, String> {
    use provenant_telemetry::ingest;
    let config = provenant_cli::config_reads::read_for(roots, "telemetry")
        .map_err(|error| error.to_string())?;
    let directories: Vec<std::path::PathBuf> = config
        .domains
        .accounts
        .entries
        .values()
        .filter(|entry| entry.adapter == ingest::PROVIDER)
        .flat_map(|entry| {
            std::iter::once(entry.directory.clone()).chain(entry.directories.iter().cloned())
        })
        .collect();
    let dirs = ingest::config_dirs(&directories, home);
    let mut chairs = std::collections::BTreeMap::new();
    for chair in usage::chair_sessions(roots, jiff::Timestamp::MIN, jiff::Timestamp::MAX)
        .map_err(|error| error.to_string())?
    {
        chairs
            .entry(chair.session)
            .or_insert(chair.account_identity);
    }
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(roots.state.as_path())
            .map_err(|error| format!("cannot create the state directory: {error}"))?;
    }
    ingest::ingest(
        roots,
        &ingest::Request {
            dirs: &dirs,
            chair_sessions: &chairs,
            provider,
            since,
            deadline,
            incremental: deadline.is_some(),
        },
    )
    .map_err(|error| error.to_string())
}

/// Wall budget for the SessionStart ingest, inside the hook's five-second host
/// timeout with room for the chair's own work.
const HOOK_INGEST_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// Best-effort ingest at chair session start and turn end.
/// The hook has a five-second host timeout, so only files modified in the last day are read; the
/// read stops at a three-second wall deadline, and the rows read so far are
/// still written. Every failure is ignored: a slow transcript must never stop
/// the chair.
#[allow(clippy::disallowed_methods)]
pub fn ingest_after_chair_hook(globals: &Globals) {
    let Ok(resolved) = roots::resolve(globals, Workspace::Directory, Product::Optional) else {
        return;
    };
    let deadline = std::time::Instant::now() + HOOK_INGEST_BUDGET;
    use provenant_core::Clock;
    let since = provenant_os::clock::SystemClock::new()
        .wall_now()
        .ok()
        .and_then(|now| jiff::Timestamp::from_second(now.as_timestamp().as_second() - 86_400).ok());
    let _ = ingest_native(&resolved.roots, &resolved.home, None, since, Some(deadline));
}

pub fn ingest(cmd: &provenant_cli::Ingest, globals: &Globals) -> u8 {
    let Ok(resolved) = roots::resolve(globals, Workspace::Directory, Product::Optional) else {
        return crate::fail(
            "telemetry ingest",
            Outcome::Error,
            "ROOT",
            "cannot resolve the state root",
            globals,
        );
    };
    match ingest_native(
        &resolved.roots,
        &resolved.home,
        cmd.provider.as_deref(),
        cmd.since,
        None,
    ) {
        Ok(report) => {
            let mut text = format!(
                "Ingested {} new of {} messages ({} already recorded) from {} directories\n",
                report.added, report.messages, report.already_present, report.directories
            );
            for (account, count) in &report.accounts {
                text.push_str(&format!("{:>8} new  {account}\n", count.added));
            }
            if report.store_full {
                text.push_str("The ingest store is full; some messages were not recorded\n");
            }
            crate::emit(
                &CommandResult::new(
                    "telemetry ingest",
                    Outcome::Ok,
                    serde_json::to_value(&report).ok(),
                ),
                globals,
                &text,
                false,
            )
        }
        Err(message) => crate::fail(
            "telemetry ingest",
            Outcome::Error,
            "INGEST",
            &message,
            globals,
        ),
    }
}

/// Record only native attempt identifiers and routing metadata.
pub fn native(cmd: provenant_cli::Native, globals: &Globals) -> u8 {
    use provenant_core::Clock;
    use provenant_telemetry::native_attempts::{self, Attempt, End, Settlement};
    let settling = matches!(&cmd, provenant_cli::Native::End { .. });
    let action = match &cmd {
        provenant_cli::Native::Start { .. } => "native start",
        provenant_cli::Native::End { .. } => "native end",
    };
    let result = (|| -> Result<(), String> {
        let resolved = roots::resolve(globals, Workspace::Directory, Product::Optional)
            .map_err(|error| error.to_string())?;
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(resolved.roots.state.as_path())
            .map_err(|error| error.to_string())?;
        let config = native_attempts::config_key(
            &resolved
                .claude_config
                .unwrap_or_else(|| resolved.home.join(".claude")),
        );
        let now = provenant_os::clock::SystemClock::new()
            .wall_now()
            .map_err(|error| error.to_string())?
            .as_timestamp()
            .to_string();
        match cmd {
            provenant_cli::Native::Start {
                id,
                session,
                alias,
                model,
                effort,
            } => native_attempts::start(
                &resolved.roots,
                Attempt {
                    id,
                    session,
                    alias,
                    model,
                    effort,
                    config,
                    started_at: now,
                    ended_at: None,
                    settlement: None,
                    agent_id: None,
                },
            ),
            provenant_cli::Native::End {
                id,
                session,
                agent,
                settlement,
            } => native_attempts::end(
                &resolved.roots,
                End {
                    config: &config,
                    session: &session,
                    id: &id,
                    ended_at: &now,
                    settlement: match settlement {
                        provenant_cli::NativeSettlement::Succeeded => Settlement::Succeeded,
                        provenant_cli::NativeSettlement::Failed => Settlement::Failed,
                        provenant_cli::NativeSettlement::Cancelled => Settlement::Cancelled,
                    },
                    agent_id: agent.as_deref(),
                },
            ),
        }
        .map_err(|error| error.to_string())
    })();
    if settling && result.is_ok() {
        ingest_after_chair_hook(globals);
    }
    match result {
        Ok(()) => crate::emit(
            &CommandResult::new(action, Outcome::Ok, serde_json::json!({"recorded": true})),
            globals,
            "recorded\n",
            false,
        ),
        Err(error) => crate::fail(action, Outcome::Error, "NATIVE_ATTEMPT", &error, globals),
    }
}

#[cfg(test)]
// The worktree driver includes this module without libtest.
#[allow(dead_code, unused_imports)]
mod tests {
    use super::*;
    use provenant_core::{PathResolver, RootInputs};
    use std::path::{Path, PathBuf};
    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
            Ok(path.to_owned())
        }
    }

    #[test]
    fn project_attribution_requires_the_knob_and_an_available_resolver() -> Result {
        let temp = tempfile::tempdir()?;
        let base = &std::fs::canonicalize(temp.path())?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir(roots.state.as_path())?;
        std::fs::set_permissions(
            roots.state.as_path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )?;
        let project = base.join("project");
        std::fs::create_dir(&project)?;
        let location = provenant_config::ProjectLocation::new(&project)?;
        let binding = provenant_config::bind_project(&roots, location)?;
        let expected_id = binding.project_id();
        let resolved = Resolved {
            assets: base.join("checkout"),
            home: base.join("home"),
            project,
            roots,
            claude_config: None,
            codex_home: None,
        };
        assert!(!attribution_enabled(&resolved.roots));
        assert_eq!(project_id(&resolved), None);
        std::fs::write(
            resolved.roots.state.as_path().join("config.toml"),
            "schema_version = 1\n[knobs]\n\"usage.project_attribution\" = true\n",
        )?;
        assert!(attribution_enabled(&resolved.roots));
        // No Git installation means best-effort attribution stays unavailable.
        let expected = provenant_os::git::git_executable().map(|_| expected_id);
        assert_eq!(project_id(&resolved), expected);
        Ok(())
    }
}
