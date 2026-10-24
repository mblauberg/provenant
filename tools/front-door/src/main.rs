// The process edge composes typed owners and owns ordinary result delivery.
mod chair;
mod checks;
mod fabric;
mod hooks;
mod hosts;
mod install;
mod overflow;
mod quota_live;
mod release_smoke;
mod roots;
mod usage;
#[cfg(test)]
#[path = "../tests/support/worktrees.rs"]
mod worktree_tests;
mod worktrees;
use provenant_cli::{Command, ParseOutcome, RenderError, RenderMode};
use provenant_records::{CommandError, CommandResult, Outcome, VersionInfo};
use serde::Serialize;
use std::{
    io::{self, Write},
    process::ExitCode,
};

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
    emit(&result, globals, "", false)
}
/// Resolve Run's composition paths, then emit the owner's reply.
fn run_edge(
    leaf: &str,
    globals: &provenant_cli::Globals,
    action: impl FnOnce(&provenant_cli::run::Edge<'_>) -> provenant_cli::run::Reply,
) -> u8 {
    let resolved = (|| -> Result<_, roots::RootError> {
        let assets = roots::product_root()?;
        let project = roots::selected_project(globals.project.as_deref(), false)?;
        let roots = roots::roots(globals, &assets, &project)?;
        Ok((roots, roots::home()?, assets, project))
    })();
    match resolved {
        Ok((roots, home, assets, cwd)) => {
            let reply = action(&provenant_cli::run::Edge {
                roots: &roots,
                home: &home,
                assets: &assets,
                cwd: &cwd,
            });
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
fn show_config(task_class: Option<&str>, globals: &provenant_cli::Globals) -> u8 {
    config_command("config show", None, task_class, globals)
}
/// Apply an optional knob edit, then print the effective config read back.
fn config_command(
    command: &str,
    edit: Option<provenant_config::KnobEdit>,
    task_class: Option<&str>,
    globals: &provenant_cli::Globals,
) -> u8 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let assets = roots::product_root()?;
        let project = roots::selected_project(globals.project.as_deref(), false)?;
        let roots = roots::roots(globals, &assets, &project)?;
        if let Some(edit) = edit {
            provenant_config::edit_config(&roots, &provenant_supervisor::fs::LocalFs, edit)?;
        }
        let mut config = provenant_config::config_show(
            &roots,
            &provenant_supervisor::fs::LocalFs,
            &provenant_cli::domains::SECTIONS,
        )?;
        if let Some(class) = task_class {
            config.apply_task_class(class)?;
        }
        Ok(config)
    })();
    match result {
        Ok(config) => match serde_json::to_string_pretty(&config) {
            Ok(text) => emit(
                &CommandResult::new(command, Outcome::Ok, config),
                globals,
                &format!("{text}\n"),
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
fn run(inv: provenant_cli::Invocation) -> u8 {
    let globals = &inv.globals;
    let command = match inv.command {
        Command::Hook(hook) => return hooks::run(hook, globals),
        command => command,
    };
    if let Err(error) = roots::validate_globals(globals) {
        // A bad selected root is a usage fault; the environment is not.
        let (status, code) = match error {
            roots::RootError::Checkout | roots::RootError::State(_) => {
                (Outcome::Invalid, "INVALID")
            }
            _ => (Outcome::Error, "ROOT"),
        };
        return fail(&inv.leaf, status, code, &error.to_string(), globals);
    }
    match command {
        Command::Hosts(command) => hosts::run(command, globals),
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
                let assets = roots::product_root()?;
                let project = roots::selected_project(inv.globals.project.as_deref(), false)?;
                let roots = roots::roots(&inv.globals, &assets, &project)?;
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
            let descriptors: Vec<_> = descriptors
                .iter()
                .map(|fd| std::ffi::OsString::from(fd.to_string()))
                .collect();
            let registry = provenant_cli::run::Registry::default();
            let holder = provenant_run::RunHolder {
                providers: &registry,
                resources: &provenant_run::SystemResources,
            };
            #[cfg(test)]
            let holder = worktree_tests::FixtureHolder(holder);
            let hooks = provenant_supervisor::GuardianHooks {
                holder: Some(&holder),
                #[cfg(test)]
                observer: Some(&provenant_supervisor::fixture::RecordedBootObserver),
                ..Default::default()
            };
            match provenant_supervisor::supervise(&descriptors, hooks) {
                Ok(()) => 0,
                Err(error) => fail(
                    "internal supervise",
                    Outcome::Error,
                    "SUPERVISOR",
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
        Command::Config(provenant_cli::Config::Show { task_class }) => {
            show_config(task_class.as_deref(), globals)
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
            None,
            globals,
        ),
        Command::Config(provenant_cli::Config::Unset { name }) => config_command(
            "config unset",
            Some(provenant_config::KnobEdit::Unset(&name)),
            None,
            globals,
        ),
        Command::Fabric(provenant_cli::Fabric::Mcp { client }) => fabric::mcp(client, globals),
        Command::Fabric(provenant_cli::Fabric::Peer) => fabric::peer(globals),
        Command::Fabric(provenant_cli::Fabric::Host(_)) => fabric::hosts(globals),
        Command::Fabric(provenant_cli::Fabric::Events(provenant_cli::fabric::Events::Read {
            page,
            wait,
        })) => fabric::events(page, wait, globals),
        Command::Fabric(provenant_cli::Fabric::Graph(graph)) => fabric::graph(graph, globals),
        Command::Fabric(provenant_cli::Fabric::Landing(
            landing @ (provenant_cli::fabric::Landing::Push { .. }
            | provenant_cli::fabric::Landing::Reconcile { .. }),
        )) => fabric::landing(landing, globals),
        Command::Fabric(cmd) => {
            let resolved = (|| -> Result<_, roots::RootError> {
                let assets = roots::product_root()?;
                let project = roots::selected_project(globals.project.as_deref(), false)?;
                let roots = roots::roots(globals, &assets, &project)?;
                Ok((roots, roots::home()?, project))
            })();
            match resolved {
                Ok((roots, home, cwd)) => {
                    provenant_cli::fabric::run(cmd, globals, &roots, &home, &cwd)
                }
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
        Command::Uninstall(args) => install::run_uninstall(args, globals),
        Command::Route(command) => {
            let resolved = (|| -> Result<_, roots::RootError> {
                let assets = roots::product_root()?;
                let project = roots::selected_project(globals.project.as_deref(), false)?;
                roots::roots(globals, &assets, &project)
            })();
            match resolved {
                Ok(roots) => provenant_cli::route::run(command, globals, &roots, |result| {
                    crate::overflow::publish(result, globals)
                }),
                Err(error) => fail(
                    &inv.leaf,
                    Outcome::Error,
                    "ROOT",
                    &error.to_string(),
                    globals,
                ),
            }
        }
        Command::Dispatch(cmd) => run_edge(&inv.leaf, globals, |edge| {
            provenant_cli::run::dispatch(*cmd, edge)
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
        Command::Reconcile(cmd) => run_edge(&inv.leaf, globals, |edge| {
            provenant_cli::run::reconcile(cmd, edge)
        }),
        Command::Telemetry(provenant_cli::Telemetry::Stats(cmd)) => {
            if cmd.usage {
                usage::run(&cmd, globals)
            } else {
                run_edge(&inv.leaf, globals, |edge| {
                    provenant_cli::run::stats(cmd, edge)
                })
            }
        }
        Command::Chair(cmd) => chair::run(cmd, globals),
        Command::Quota(cmd) => {
            let resolved = (|| -> Result<_, roots::RootError> {
                let assets = roots::product_root()?;
                let project = roots::selected_project(globals.project.as_deref(), false)?;
                roots::roots(globals, &assets, &project)
            })();
            match resolved {
                Ok(roots) => match quota_live::LiveQuota::new(roots.state.as_path()) {
                    Ok(effects) => provenant_cli::quota::run(
                        cmd,
                        globals,
                        &roots,
                        provenant_providers::REGISTRY,
                        &effects,
                        |result| crate::overflow::publish(result, globals),
                    ),
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
        Command::Workflow(cmd) => {
            let resolved = (|| -> Result<_, roots::RootError> {
                let assets = roots::product_root()?;
                let project = roots::selected_project(globals.project.as_deref(), false)?;
                Ok((
                    roots::roots(globals, &assets, &project)?,
                    roots::home()?,
                    project,
                ))
            })();
            match resolved {
                Ok((roots, home, cwd)) => {
                    provenant_cli::workflow::run(cmd, globals, &roots, &home, &cwd)
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
        Command::Worktree(worktree) => worktrees::run(worktree, globals),
        Command::Check(check) => checks::run(check, globals),
        Command::Hook(hook) => hooks::run(hook, globals),
    }
}
fn main() -> ExitCode {
    #[cfg(test)]
    if std::env::args_os()
        .nth(1)
        .as_deref()
        .is_some_and(|arg| arg == "exec" || arg == "-p")
    {
        return match worktree_tests::lane_provider() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "lane provider: {error}");
                ExitCode::FAILURE
            }
        };
    }
    #[cfg(test)]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--writer-provider")) {
        return match worktree_tests::provider(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "writer provider: {error}");
                ExitCode::FAILURE
            }
        };
    }
    // The integration target compiles this command edge unchanged apart from
    // native worker observations. Its children re-enter the normal parser.
    #[cfg(test)]
    if !matches!(std::env::args_os().nth(1).as_deref(), Some(arg) if arg == "--state-dir" || arg == "internal")
    {
        return match worktree_tests::suite(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "worktree integration: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let started = std::time::Instant::now();
    let parsed = provenant_cli::parse(std::env::args_os().skip(1));
    let (verb, globals) = match &parsed {
        ParseOutcome::Invocation(inv) => (inv.leaf.as_str(), &inv.globals),
        ParseOutcome::Invalid { globals, .. } => ("parse", globals),
        ParseOutcome::Help { globals, .. } => ("help", globals),
        ParseOutcome::Version { globals } => ("version", globals),
    };
    let verb = verb.to_owned();
    let usage_roots = usage::selected_roots(globals);
    let status = match parsed {
        ParseOutcome::Invocation(inv) => run(inv),
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
    if let Some(roots) = usage_roots {
        usage::record_cli(
            &roots,
            &verb,
            status,
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
    }
    ExitCode::from(status)
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn git_resolver_prefers_override_then_candidates_and_checks_executable() {
        let root = tempfile::tempdir().expect("temp dir");
        let override_dir = root.path().join("override");
        let path_dir = root.path().join("path");
        fs::create_dir_all(&override_dir).expect("override dir");
        fs::create_dir_all(&path_dir).expect("path dir");
        let override_git = override_dir.join("git");
        let path_git = path_dir.join("git");
        fs::write(&override_git, b"git").expect("override binary");
        fs::write(&path_git, b"git").expect("path binary");
        fs::set_permissions(&override_git, fs::Permissions::from_mode(0o700))
            .expect("override mode");
        fs::set_permissions(&path_git, fs::Permissions::from_mode(0o700)).expect("path mode");

        assert_eq!(
            crate::roots::resolve_git_binary(None, &[path_dir.join("missing"), path_git.clone()]),
            Some(path_git.clone())
        );
        assert_eq!(
            crate::roots::resolve_git_binary(Some(&override_git), &[path_git]),
            Some(override_git)
        );
    }
}
