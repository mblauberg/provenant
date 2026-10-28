// The process edge composes typed owners and owns ordinary result delivery.
// `main.rs` parses and calls `dispatch`; the worktree driver includes this
// file and dispatches with its fixture seams. Plain comments only: `include!`
// refuses inner attributes.
mod bench;
mod chair;
mod checks;
#[cfg(test)]
mod compose_tests;
mod console;
mod disk;
mod disk_guard;
mod fabric;
mod gc_agent;

mod chair_log_retention;
mod gc_support;
mod handoff;
mod hook_state;
mod hook_worktree;
mod hooks;
mod hosts;
mod install;
mod overflow;
mod pr;
#[cfg(test)]
mod pr_land_tests;
mod provider_retention;
mod release_smoke;
mod roots;
mod usage;
#[cfg(test)]
pub mod worktree_tests;
mod worktrees;
use provenant_cli::{Command, ParseOutcome, RenderError, RenderMode};
use provenant_records::{CommandError, CommandResult, Outcome, VersionInfo};
use roots::{Product, Workspace};
use serde::Serialize;
use std::{
    ffi::OsString,
    io::{self, Write},
};

/// Process-wide seams the entry point selects once. The defaults observe the
/// real system; the worktree driver substitutes its fixtures.
pub trait Process {
    /// Observes whether a worktree's writer is alive.
    fn workers(&self) -> &'static dyn provenant_confinement::worktrees::WorkerObserver {
        &provenant_confinement::worktrees::SystemWorkerObserver
    }
    /// Hands Run's terminal holder to the guardian composition.
    fn holder(
        &self,
        holder: &dyn provenant_supervisor::Holder,
        guard: &mut dyn FnMut(&dyn provenant_supervisor::Holder) -> u8,
    ) -> u8 {
        guard(holder)
    }
    /// Runs the guardian with the composed hooks.
    fn supervise(
        &self,
        descriptors: &[OsString],
        hooks: provenant_supervisor::GuardianHooks<'_>,
    ) -> provenant_supervisor::custody::Result<()> {
        provenant_supervisor::supervise(descriptors, hooks)
    }
}
/// The installed binary's seams.
pub struct System;
impl Process for System {}

fn identity() -> Result<VersionInfo, Box<dyn std::error::Error>> {
    Ok(VersionInfo {
        source_digest: provenant_records::SourceDigest::parse(env!("PROVENANT_SOURCE_DIGEST"))?,
        asset_digest: provenant_records::SourceDigest::parse(env!("PROVENANT_ASSET_DIGEST"))?,
        target: env!("PROVENANT_TARGET_TRIPLE").into(),
        toolchain: env!("PROVENANT_TOOLCHAIN").into(),
        record_schema_version: 1,
        config_schema_version: provenant_config::CONFIG_SCHEMA_VERSION,
    })
}
fn human_inbox(
    command: provenant_cli::inbox::HumanCommand,
    path: &str,
    globals: &provenant_cli::Globals,
) -> u8 {
    match roots::resolve(globals, Workspace::Project, Product::Required) {
        Ok(resolved) => provenant_cli::inbox::run(
            command,
            path,
            globals,
            &resolved.roots,
            &resolved.home,
            &resolved.project,
        ),
        Err(error) => fail(path, Outcome::Error, "ROOT", &error.to_string(), globals),
    }
}
fn emit<T: Serialize>(
    result: &CommandResult<T>,
    globals: &provenant_cli::Globals,
    human: &str,
    read_only: bool,
) -> u8 {
    let mode = if globals.json {
        RenderMode::Json
    } else {
        RenderMode::Human {
            text: human,
            read_only,
        }
    };
    let rendered = provenant_cli::render_or_publish(
        result,
        mode,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        |value| overflow::publish(value, globals),
    );
    match rendered {
        Ok(status) => provenant_cli::exit_status(&status),
        Err(RenderError::TooLarge) => fail(
            &result.command,
            Outcome::Error,
            "RESULT_TOO_LARGE",
            "result exceeds 1 MiB; request a smaller page or read the owner-published artifact",
            globals,
        ),
        Err(_) => {
            let _ = io::stderr().write_all(b"provenant: IO: cannot write command result\n");
            3
        }
    }
}
fn fail(
    command: &str,
    status: Outcome,
    code: &str,
    message: &str,
    globals: &provenant_cli::Globals,
) -> u8 {
    let mut result = CommandResult::new(command, status, ());
    result.error = Some(CommandError {
        code: code.into(),
        message: message.into(),
        cause: None,
        repair: None,
    });
    if let Some(error) = &mut result.error {
        apply_repair(command, error, globals.json);
    }
    emit(&result, globals, "", false)
}
fn apply_repair(command: &str, error: &mut CommandError, json: bool) {
    if error.repair.is_some() {
        return;
    }
    error.repair = match (command, error.code.as_str()) {
        ("install", "HOME") => Some("Set HOME to your absolute home directory, then rerun `provenant install`.".into()),
        ("gc", "GC") => Some("Run `provenant gc --apply --json` to inspect failed paths and retry incomplete collection.".into()),
        (_, _) if error.message.starts_with("insufficient disk space") => {
            // The CLI owner still renders its old floor override. Keep the
            // observed volume and floor, and give the actionable command here.
            if let Some((figures, _)) = error.message.split_once("; run `") {
                error.message = format!("{figures}; run `provenant gc --apply`");
            }
            Some("Run `provenant gc --apply`, then retry when free space reaches gc.free_floor.".into())
        }
        _ => None,
    };
    if !json && let Some(repair) = &error.repair {
        error.message.push_str(&format!("; {repair}"));
    }
}
/// Resolve Run's composition paths, then emit the owner's reply.
fn run_edge(
    leaf: &str,
    globals: &provenant_cli::Globals,
    action: impl FnOnce(&provenant_cli::run::Edge<'_>) -> provenant_cli::run::Reply,
) -> u8 {
    run_edge_workspace(leaf, globals, Workspace::Project, action)
}
fn run_edge_workspace(
    leaf: &str,
    globals: &provenant_cli::Globals,
    workspace: Workspace<'_>,
    action: impl FnOnce(&provenant_cli::run::Edge<'_>) -> provenant_cli::run::Reply,
) -> u8 {
    match roots::resolve(globals, workspace, Product::Required) {
        Ok(resolved) => {
            let mut reply = action(&provenant_cli::run::Edge {
                roots: &resolved.roots,
                home: &resolved.home,
                assets: &resolved.assets,
                cwd: &resolved.project,
            });
            if let Some(error) = &mut reply.result.error {
                apply_repair(leaf, error, globals.json);
            }
            emit(&reply.result, globals, &reply.human, false)
        }
        Err(error) => {
            let status = match error {
                roots::RootError::Checkout | roots::RootError::State(_) => Outcome::Invalid,
                _ => Outcome::Error,
            };
            fail(leaf, status, "ROOT", &error.to_string(), globals)
        }
    }
}
/// Apply an optional knob edit, then print the effective config read back.
fn config_command(
    command: &str,
    edit: Option<provenant_config::KnobEdit>,
    globals: &provenant_cli::Globals,
) -> u8 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let roots = roots::resolve(globals, Workspace::Project, Product::Required)?.roots;
        let sections = &provenant_cli::domains::SECTIONS;
        Ok(match edit {
            Some(edit) => provenant_config::edit_config(&roots, sections, |doc| edit.apply(doc))?,
            None => provenant_config::config_show(&roots, sections)?,
        })
    })();
    match result {
        Ok(config) => match serde_json::to_value(&config) {
            Ok(value) => emit(
                &CommandResult::new(command, Outcome::Ok, config),
                globals,
                &provenant_cli::terse(&value),
                true,
            ),
            Err(_) => fail(
                command,
                Outcome::Error,
                "ENCODE",
                "cannot encode config",
                globals,
            ),
        },
        Err(error) => {
            let status = match error.downcast_ref::<provenant_config::ConfigError>() {
                Some(
                    provenant_config::ConfigError::Read { .. }
                    | provenant_config::ConfigError::ProjectBindingFilesystem(_),
                )
                | None => Outcome::Error,
                Some(_) => Outcome::Invalid,
            };
            fail(command, status, "CONFIG", &error.to_string(), globals)
        }
    }
}
/// Registered account settings, read through the binary's domain aggregate.
fn account_settings(
    roots: &provenant_core::Roots,
) -> Result<provenant_providers::accounts::Settings, provenant_config::ConfigError> {
    provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS)
        .map(|config| config.domains.accounts)
}
/// Explicit Claude registrations only, never implicit directory discovery.
pub(crate) fn registered_claude(
    roots: &provenant_core::Roots,
) -> Result<Vec<provenant_providers::identity::RegisteredAccount>, Box<dyn std::error::Error>> {
    provenant_providers::quota_live::registered_claude(&account_settings(roots)?)
}
fn live_quota(
    resolved: &roots::Resolved,
) -> Result<provenant_providers::quota_live::LiveQuota, Box<dyn std::error::Error>> {
    let accounts = account_settings(&resolved.roots)?;
    Ok(provenant_providers::quota_live::LiveQuota::new(
        resolved.roots.state.as_path(),
        &resolved.home,
        &accounts,
    )?)
}
/// The LaunchAgent's long-running entrypoint, `fabric peer-agent`.
fn peer_agent(globals: &provenant_cli::Globals) -> u8 {
    use provenant_fabric_runtime::peer_agent;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        if provenant_cli::fabric::in_lane() {
            return Err(
                provenant_fabric_runtime::coordination::CoordinationError::LaneMutation.into(),
            );
        }
        let resolved = roots::resolve(globals, Workspace::Home, Product::Required)?;
        Ok(peer_agent::serve(
            &std::env::current_exe()?,
            resolved.roots.state.as_path(),
            &resolved.home,
        )?)
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            peer_agent::log(&error.to_string());
            3
        }
    }
}
/// The guardian entry for one launched attempt, then any settled graph step.
fn supervise(descriptors: &[u64], globals: &provenant_cli::Globals, process: &dyn Process) -> u8 {
    let descriptors: Vec<_> = descriptors
        .iter()
        .map(|fd| OsString::from(fd.to_string()))
        .collect();
    let registry = provenant_cli::run::Registry::default();
    let holder = provenant_run::RunHolder {
        providers: &registry,
        resources: &provenant_run::SystemResources,
    };
    process.holder(&holder, &mut |holder| {
        let holder = provenant_cli::run::HostHolder::new(holder);
        let disk = disk_guard::LaneDisk::default();
        let hooks = provenant_supervisor::GuardianHooks {
            disk: Some(&disk),
            holder: Some(&holder),
            nested_children: Some(&holder),
            ..Default::default()
        };
        match process.supervise(&descriptors, hooks) {
            Ok(()) => match fabric::advance_settled_graphs(&holder, process.workers()) {
                Ok(()) => 0,
                Err(error) => fail(
                    "internal supervise",
                    Outcome::Error,
                    "GRAPH_ADVANCE",
                    &error.to_string(),
                    globals,
                ),
            },
            Err(error) => fail(
                "internal supervise",
                Outcome::Error,
                "SUPERVISOR",
                &error.to_string(),
                globals,
            ),
        }
    })
}
fn run(inv: provenant_cli::Invocation, process: &dyn Process) -> u8 {
    let globals = &inv.globals;
    let command = match inv.command {
        Command::Hook(hook) => return hooks::run(hook, globals),
        command => command,
    };
    if let Err(error) = roots::validate_globals(globals) {
        // A bad selected root is a usage fault; the environment is not.
        let (status, code) = match error {
            roots::RootError::Home if inv.leaf == "install" => (Outcome::Invalid, "HOME"),
            roots::RootError::Checkout | roots::RootError::State(_) => {
                (Outcome::Invalid, "INVALID")
            }
            _ => (Outcome::Error, "ROOT"),
        };
        return fail(&inv.leaf, status, code, &error.to_string(), globals);
    }
    match command {
        Command::Hosts { command: None } => fabric::hosts(globals, process.workers()),
        Command::Hosts {
            command: Some(command),
        } => hosts::run(command, globals),
        Command::Accounts(command) => {
            run_edge(
                &inv.leaf,
                globals,
                |edge| match provenant_cli::accounts::execute(&command, edge.roots, edge.home) {
                    Ok(data) => provenant_cli::run::Reply {
                        human: data
                            .entries
                            .values()
                            .map(|e| format!("{} {} {}\n", e.adapter, e.label, e.key))
                            .collect(),
                        result: CommandResult::new(&inv.leaf, Outcome::Ok, serde_json::json!(data)),
                    },
                    Err(message) => {
                        let mut result = CommandResult::new(
                            &inv.leaf,
                            Outcome::Refused,
                            serde_json::Value::Null,
                        );
                        result.error = Some(CommandError {
                            code: "ACCOUNT".into(),
                            message,
                            cause: None,
                            repair: None,
                        });
                        provenant_cli::run::Reply {
                            result,
                            human: String::new(),
                        }
                    }
                },
            )
        }
        Command::Root => {
            #[derive(Serialize)]
            struct RootInfo {
                assets: provenant_records::AbsolutePath,
                state_directory: provenant_records::AbsolutePath,
                project: provenant_records::AbsolutePath,
                package_version: &'static str,
                installation: VersionInfo,
            }
            let result = (|| -> Result<_, Box<dyn std::error::Error>> {
                let roots::Resolved {
                    assets,
                    project,
                    roots,
                    ..
                } = roots::resolve(&inv.globals, Workspace::Project, Product::Required)?;
                let data = RootInfo {
                    assets: provenant_records::AbsolutePath::new(&assets)?,
                    state_directory: provenant_records::AbsolutePath::new(roots.state.as_path())?,
                    project: provenant_records::AbsolutePath::new(&project)?,
                    package_version: env!("CARGO_PKG_VERSION"),
                    installation: identity()?,
                };
                Ok((
                    data,
                    match assets.to_str() {
                        Some(path) => format!("{path}\n"),
                        None => {
                            format!("{}\n", assets.as_os_str().as_encoded_bytes().escape_ascii())
                        }
                    },
                ))
            })();
            match result {
                Ok((data, human)) => emit(
                    &CommandResult::new("root", Outcome::Ok, data),
                    globals,
                    &human,
                    true,
                ),
                Err(error) => fail("root", Outcome::Error, "ROOT", &error.to_string(), globals),
            }
        }
        Command::Internal(provenant_cli::Internal::Supervise { descriptors }) => {
            supervise(&descriptors, globals, process)
        }
        Command::Internal(provenant_cli::Internal::ProviderUpdates) => {
            match roots::resolve(globals, Workspace::Project, Product::Required) {
                Ok(resolved) => match provenant_cli::provider_updates::housekeep(
                    &resolved.roots,
                    &resolved.home,
                    &resolved.project,
                ) {
                    Ok(()) => 0,
                    Err(error) => fail(
                        &inv.leaf,
                        Outcome::Error,
                        "PROVIDER_UPDATE",
                        &error,
                        globals,
                    ),
                },
                Err(error) => fail(
                    &inv.leaf,
                    Outcome::Error,
                    "ROOT",
                    &error.to_string(),
                    globals,
                ),
            }
        }
        Command::Help { .. } => fail(
            "help",
            Outcome::Invalid,
            "INVALID",
            "help must be resolved by the parser",
            globals,
        ),
        Command::Config(provenant_cli::Config::Show) => {
            config_command("config show", None, globals)
        }
        Command::Config(provenant_cli::Config::Set { name, value }) => config_command(
            "config set",
            Some(if name == "profile" {
                provenant_config::KnobEdit::Profile(&value)
            } else {
                provenant_config::KnobEdit::Set {
                    name: &name,
                    value: &value,
                }
            }),
            globals,
        ),
        Command::Config(provenant_cli::Config::Unset { name }) => config_command(
            "config unset",
            Some(provenant_config::KnobEdit::Unset(&name)),
            globals,
        ),
        Command::Ask(args) => human_inbox(
            provenant_cli::inbox::HumanCommand::Ask(args),
            &inv.leaf,
            globals,
        ),
        Command::Inbox(args) => human_inbox(
            provenant_cli::inbox::HumanCommand::Inbox(args),
            &inv.leaf,
            globals,
        ),
        Command::Answer(args) => human_inbox(
            provenant_cli::inbox::HumanCommand::Answer(args),
            &inv.leaf,
            globals,
        ),
        Command::Fabric(provenant_cli::Fabric::Mcp { client }) => {
            fabric::mcp(client, globals, process.workers())
        }
        Command::Fabric(provenant_cli::Fabric::Peer { in_process }) => {
            fabric::peer(globals, in_process, process.workers())
        }
        Command::Fabric(provenant_cli::Fabric::PeerAgent) => peer_agent(globals),
        Command::Fabric(provenant_cli::Fabric::Events(provenant_cli::fabric::Events::Read {
            page,
            wait,
        })) => fabric::events(page, wait, globals, process.workers()),
        Command::Fabric(provenant_cli::Fabric::Graph(graph)) => {
            fabric::graph(graph, globals, process.workers())
        }
        Command::Fabric(provenant_cli::Fabric::Landing(
            landing @ (provenant_cli::fabric::Landing::Push { .. }
            | provenant_cli::fabric::Landing::Reconcile { .. }),
        )) => fabric::landing(landing, globals, process.workers()),
        Command::Fabric(cmd) => {
            match roots::resolve(globals, Workspace::Project, Product::Required) {
                Ok(resolved) => provenant_cli::fabric::run(
                    cmd,
                    globals,
                    &resolved.roots,
                    &resolved.home,
                    &resolved.project,
                ),
                Err(error) => {
                    let status = match error {
                        roots::RootError::Checkout | roots::RootError::State(_) => Outcome::Invalid,
                        _ => Outcome::Error,
                    };
                    fail(&inv.leaf, status, "ROOT", &error.to_string(), globals)
                }
            }
        }
        Command::Install(args) => install::run_install(args, globals),
        Command::Upgrade(args) => install::run_upgrade(args, globals),
        Command::Uninstall(args) => install::run_uninstall(args, globals),
        Command::Route(command) => {
            match roots::resolve(globals, Workspace::Project, Product::Required) {
                Ok(resolved) => {
                    provenant_cli::route::run(command, globals, &resolved.roots, |result| {
                        crate::overflow::publish(result, globals)
                    })
                }
                Err(error) => fail(
                    &inv.leaf,
                    Outcome::Error,
                    "ROOT",
                    &error.to_string(),
                    globals,
                ),
            }
        }
        Command::Run(cmd) => run_edge(&inv.leaf, globals, |edge| {
            let mut reply = provenant_cli::run::run(*cmd, edge);
            if let Err(error) =
                fabric::settle_dispatched_attempts(edge, &mut reply, process.workers())
            {
                reply
                    .human
                    .push_str(&format!("worktree retained: {error}\n"));
                reply.result.data["worktree_settlement_error"] =
                    serde_json::json!(error.to_string());
            }
            reply
        }),
        Command::Status(cmd) => run_edge(&inv.leaf, globals, |edge| {
            provenant_cli::run::status(cmd, globals.json, edge)
        }),
        Command::Land { number, head } => pr::land::run(number, head, globals, process.workers()),
        Command::Dispatch(cmd) => run_edge(&inv.leaf, globals, |edge| {
            let mut reply = provenant_cli::run::dispatch(*cmd, edge);
            if let Err(error) =
                fabric::settle_dispatched_attempts(edge, &mut reply, process.workers())
            {
                reply
                    .human
                    .push_str(&format!("worktree retained: {error}\n"));
                reply.result.data["worktree_settlement_error"] =
                    serde_json::json!(error.to_string());
            }
            reply
        }),
        Command::Lanes(cmd) if cmd.watch.is_some() => run_edge(&inv.leaf, globals, |edge| {
            provenant_cli::run::lanes_streaming(
                cmd,
                edge,
                globals.json,
                &mut provenant_cli::run::write_line,
            )
        }),
        Command::Lanes(cmd) => run_edge(&inv.leaf, globals, |edge| {
            provenant_cli::run::lanes(cmd, edge)
        }),
        Command::Output(cmd) => run_edge(&inv.leaf, globals, |edge| {
            provenant_cli::run::output(cmd, edge)
        }),
        Command::Cancel(cmd) => run_edge(&inv.leaf, globals, |edge| {
            provenant_cli::run::cancel(cmd, edge)
        }),
        Command::Resume(cmd) => run_edge(&inv.leaf, globals, |edge| {
            provenant_cli::run::resume(cmd, edge)
        }),
        Command::Lane(cmd) => run_edge(&inv.leaf, globals, |edge| {
            let mut reply = provenant_cli::run::lane(cmd, edge);
            if let Err(error) =
                fabric::settle_delivered_attempt(edge, &mut reply, process.workers())
            {
                reply
                    .human
                    .push_str(&format!("worktree retained: {error}\n"));
                reply.result.data["worktree_settlement_error"] =
                    serde_json::json!(error.to_string());
            }
            reply
        }),
        Command::Reconcile(cmd) => run_edge(&inv.leaf, globals, |edge| {
            provenant_cli::run::reconcile(cmd, edge)
        }),
        Command::Telemetry(provenant_cli::Telemetry::Stats(cmd)) => {
            if cmd.usage {
                usage::run(&cmd, globals)
            } else {
                let workspace = if cmd.claude_subagents.is_some() {
                    Workspace::Directory
                } else {
                    Workspace::Project
                };
                run_edge_workspace(&inv.leaf, globals, workspace, |edge| {
                    provenant_cli::run::stats(cmd, edge)
                })
            }
        }
        Command::Telemetry(provenant_cli::Telemetry::Trace(cmd)) => {
            run_edge(&inv.leaf, globals, |edge| {
                provenant_cli::run::trace(cmd, edge)
            })
        }
        Command::Chair(cmd) => chair::run(cmd, globals),
        Command::Console(cmd) => console::run(cmd, globals),
        Command::Quota { command } => {
            let cmd = command.unwrap_or(provenant_cli::Quota::Show(provenant_cli::quota::Filter {
                adapter: None,
            }));
            match roots::resolve(globals, Workspace::Project, Product::Required) {
                Ok(resolved) => match live_quota(&resolved) {
                    Ok(effects) => {
                        let housekeeping = matches!(cmd, provenant_cli::Quota::Tick { .. });
                        let result = provenant_cli::quota::run(
                            cmd,
                            globals,
                            &resolved.roots,
                            provenant_providers::REGISTRY,
                            &effects,
                            |result| crate::overflow::publish(result, globals),
                        );
                        if housekeeping {
                            let _ = provenant_cli::provider_updates::schedule(
                                &resolved.roots,
                                &resolved.project,
                            );
                        }
                        result
                    }
                    Err(_) => fail(
                        &inv.leaf,
                        Outcome::Invalid,
                        "QUOTA_CONFIG",
                        "invalid quota accounts configuration",
                        globals,
                    ),
                },
                Err(error) => fail(
                    &inv.leaf,
                    Outcome::Error,
                    "ROOT",
                    &error.to_string(),
                    globals,
                ),
            }
        }
        Command::Workflow(provenant_cli::Workflow::Session(
            provenant_cli::workflow::Session::Handoff(args),
        )) => handoff::run(*args, globals, process.workers()),
        Command::Workflow(cmd) => {
            match roots::resolve(globals, Workspace::Project, Product::Required) {
                Ok(resolved) => {
                    let state = resolved.roots.state.as_path().to_owned();
                    let home = resolved.home.clone();
                    let project = resolved.project.clone();
                    provenant_cli::workflow::run(
                        cmd,
                        globals,
                        &resolved.roots,
                        &resolved.home,
                        &resolved.project,
                        move || {
                            bounded_session_warning(move || {
                                disk_guard::session_warning(&state, &home, &project)
                            })
                        },
                    )
                }
                Err(error) => fail(
                    &inv.leaf,
                    Outcome::Error,
                    "ROOT",
                    &error.to_string(),
                    globals,
                ),
            }
        }
        Command::Worktree(worktree) => worktrees::run(worktree, globals, process.workers()),
        Command::Pr(command) => pr::run(command, globals, process.workers()),
        Command::Gc(command) => worktrees::run_gc(command, globals, process.workers()),
        Command::Disk(provenant_cli::Disk::Report) => disk::run(globals),
        Command::Bench(command) => bench::run(command, globals),
        Command::Check(check) => checks::run(check, globals),
        Command::Hook(hook) => hooks::run(hook, globals),
    }
}

/// Disk observations are best-effort and may stall on an unhealthy volume.
fn bounded_session_warning(
    observe: impl FnOnce() -> Option<String> + Send + 'static,
) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("session-disk-warning".into())
        .spawn(move || {
            let _ = tx.send(observe());
        })
        .ok()?;
    rx.recv_timeout(std::time::Duration::from_millis(250))
        .ok()
        .flatten()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod session_warning_tests {
    #[test]
    fn stalled_observation_does_not_hold_save() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let warning = super::bounded_session_warning(move || {
            let _ = rx.recv();
            Some("late".to_owned())
        });
        assert_eq!(warning, None);
        drop(tx);
    }
}
/// Run one parsed command line and count it, returning the exit status.
// CLI reporting measures real elapsed time for operational diagnostics.
#[allow(clippy::disallowed_methods)]
pub fn dispatch(parsed: ParseOutcome, process: &dyn Process) -> u8 {
    let started = std::time::Instant::now();
    let (verb, globals) = match &parsed {
        ParseOutcome::Invocation(inv) => (inv.leaf.as_str(), &inv.globals),
        ParseOutcome::Invalid { globals, .. } => ("parse", globals),
        ParseOutcome::Help { globals, .. } => ("help", globals),
        ParseOutcome::Version { globals } => ("version", globals),
    };
    let verb = verb.to_owned();
    // No product installation, Git probe or provider access is needed to count.
    // Read-only catalogue and checkpoint observation must not record demand
    // telemetry, including on an otherwise empty state root.
    let usage_roots = if matches!(
        verb.as_str(),
        "workflow session show"
            | "workflow session diff"
            | "console projects"
            | "console sessions"
            | "console overview"
    ) {
        None
    } else {
        roots::resolve(globals, Workspace::Directory, Product::Optional).ok()
    };
    let usage_project = usage_roots.as_ref().and_then(usage::project_id);
    let status = match parsed {
        ParseOutcome::Invocation(inv) => run(inv, process),
        ParseOutcome::Invalid { error, globals } => fail(
            "parse",
            Outcome::Invalid,
            "INVALID",
            &error.to_string(),
            &globals,
        ),
        ParseOutcome::Help {
            text,
            catalog,
            globals,
        } => emit(
            &CommandResult::new("help", Outcome::Ok, catalog),
            &globals,
            &text,
            true,
        ),
        ParseOutcome::Version { globals } => match identity() {
            Ok(identity) => {
                #[derive(Serialize)]
                struct BuildIdentity {
                    package_version: &'static str,
                    #[serde(flatten)]
                    installation: VersionInfo,
                }
                emit(
                    &CommandResult::new(
                        "version",
                        Outcome::Ok,
                        BuildIdentity {
                            package_version: env!("CARGO_PKG_VERSION"),
                            installation: identity,
                        },
                    ),
                    &globals,
                    concat!(
                        "provenant ",
                        env!("CARGO_PKG_VERSION"),
                        " ",
                        env!("PROVENANT_SOURCE_DIGEST"),
                        "\n"
                    ),
                    true,
                )
            }
            Err(error) => fail(
                "version",
                Outcome::Error,
                "IDENTITY",
                &error.to_string(),
                &globals,
            ),
        },
    };
    if let Some(resolved) = usage_roots {
        provenant_telemetry::usage::record_cli(
            &resolved.roots,
            &provenant_os::clock::SystemClock,
            &verb,
            status,
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            usage_project,
        );
    }
    status
}
