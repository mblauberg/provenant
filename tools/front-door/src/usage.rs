//! Local demand counters. The command edge decides what to count and renders
//! the report; telemetry owns the log.
use crate::roots::{self, Product, Resolved, Workspace};
use provenant_cli::{Globals, Stats};
use provenant_core::{ProjectId, Roots};
use provenant_records::{CommandResult, Outcome};
use provenant_telemetry::usage::{self, EVENT_KINDS};

pub fn attribution_enabled(roots: &Roots) -> bool {
    provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS)
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

/// Every installed CLI verb and MCP tool, so unused units report zero calls.
fn inventory() -> Vec<(String, String)> {
    provenant_cli::usage_verbs()
        .into_iter()
        .map(|verb| (EVENT_KINDS[0].into(), verb))
        .chain(
            provenant_fabric_mcp::tools::catalogue()
                .iter()
                .filter_map(|tool| {
                    tool["name"]
                        .as_str()
                        .map(|name| (EVENT_KINDS[1].into(), name.into()))
                }),
        )
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
/// accounts attribute their directories; every other `~/.claude*` directory is
/// `unattributed`, never dropped.
pub fn ingest_native(
    roots: &Roots,
    home: &std::path::Path,
    provider: Option<&str>,
    since: Option<jiff::Timestamp>,
) -> Result<provenant_telemetry::ingest::Report, String> {
    use provenant_telemetry::ingest;
    let config = provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS)
        .map_err(|error| error.to_string())?;
    let accounts: Vec<(String, Vec<std::path::PathBuf>)> = config
        .domains
        .accounts
        .entries
        .values()
        .filter(|entry| entry.adapter == ingest::PROVIDER)
        .map(|entry| {
            let mut directories = vec![entry.directory.clone()];
            directories.extend(entry.directories.iter().cloned());
            (entry.label.clone(), directories)
        })
        .collect();
    let dirs = ingest::config_dirs(&accounts, home);
    let chairs: std::collections::BTreeSet<String> =
        usage::chair_sessions(roots, jiff::Timestamp::MIN, jiff::Timestamp::MAX)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|chair| chair.session)
            .collect();
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
        },
    )
    .map_err(|error| error.to_string())
}

/// Best-effort ingest after the chair's SessionStart hook records its session.
/// The hook has a five-second host timeout, so only the last day is read and
/// every failure is ignored: a slow transcript must never stop the chair.
pub fn ingest_after_chair_hook(globals: &Globals) {
    let Ok(resolved) = roots::resolve(globals, Workspace::Directory, Product::Optional) else {
        return;
    };
    use provenant_core::Clock;
    let since = provenant_os::clock::SystemClock::new()
        .wall_now()
        .ok()
        .and_then(|now| jiff::Timestamp::from_second(now.as_timestamp().as_second() - 86_400).ok());
    let _ = ingest_native(&resolved.roots, &resolved.home, None, since);
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
