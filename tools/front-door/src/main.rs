// The process edge composes typed owners and owns ordinary result delivery.
mod checks;
mod hooks;
mod hosts;
mod install;
mod overflow;
mod roots;
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
fn unavailable(command: &str, owner: &'static str, globals: &provenant_cli::Globals) -> u8 {
    let error = provenant_cli::CliError::OwnerUnavailable { owner };
    fail(
        command,
        Outcome::Unavailable,
        "OWNER_UNAVAILABLE",
        &error.to_string(),
        globals,
    )
}
fn show_config(globals: &provenant_cli::Globals) -> u8 {
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let assets = roots::product_root()?;
        let project = roots::selected_project(globals.project.as_deref(), false)?;
        let roots = roots::roots(globals, &assets, &project)?;
        Ok(provenant_config::config_show(
            &roots,
            &provenant_supervisor::fs::LocalFs,
            &provenant_cli::domains::SECTIONS,
        )?)
    })();
    match result {
        Ok(config) => match serde_json::to_string_pretty(&config) {
            Ok(text) => emit(
                &CommandResult::new("config show", Outcome::Ok, config),
                globals,
                &format!("{text}\n"),
                true,
            ),
            Err(_) => fail(
                "config show",
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
            fail("config show", status, "CONFIG", &error.to_string(), globals)
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
            match provenant_supervisor::supervise(&descriptors, Default::default()) {
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
        Command::Config(provenant_cli::Config::Show) => show_config(globals),
        Command::Fabric(provenant_cli::Fabric::Mcp { .. }) => {
            unavailable("fabric mcp", "fabric", globals)
        }
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
                Ok(roots) => provenant_cli::route::run(command, globals, &roots),
                Err(error) => fail(
                    &inv.leaf,
                    Outcome::Error,
                    "ROOT",
                    &error.to_string(),
                    globals,
                ),
            }
        }
        Command::Dispatch(_) => unavailable("dispatch", "run", globals),
        Command::Lanes(_) => unavailable("lanes", "run", globals),
        Command::Output(_) => unavailable("output", "run", globals),
        Command::Cancel(_) => unavailable("cancel", "run", globals),
        Command::Resume(_) => unavailable("resume", "run", globals),
        Command::Reconcile(_) => unavailable("reconcile", "run", globals),
        Command::Telemetry(provenant_cli::Telemetry::Stats(_)) => {
            unavailable("telemetry stats", "telemetry", globals)
        }
        Command::Quota(cmd) => {
            let resolved = (|| -> Result<_, roots::RootError> {
                let assets = roots::product_root()?;
                let project = roots::selected_project(globals.project.as_deref(), false)?;
                roots::roots(globals, &assets, &project)
            })();
            match resolved {
                Ok(roots) => {
                    provenant_cli::quota::run(cmd, globals, &roots, provenant_providers::REGISTRY)
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
    // The integration target compiles this command edge unchanged apart from
    // native worker observations. Its children re-enter the normal parser.
    #[cfg(test)]
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("--state-dir")) {
        return match worktree_tests::suite(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "worktree integration: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let status = match provenant_cli::parse(std::env::args_os().skip(1)) {
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
    ExitCode::from(status)
}
