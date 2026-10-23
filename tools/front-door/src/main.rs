//! The process edge composes typed owners and owns ordinary result delivery.
#![forbid(unsafe_code)]
mod overflow;
mod roots;
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
fn emit<T: Serialize>(result: &CommandResult<T>, json: bool, human: &str, read_only: bool) -> u8 {
    emit_bound(result, json, human, read_only, None)
}
fn emit_bound<T: Serialize>(
    result: &CommandResult<T>,
    json: bool,
    human: &str,
    read_only: bool,
    globals: Option<&provenant_cli::Globals>,
) -> u8 {
    let mode = if json {
        RenderMode::Json
    } else {
        RenderMode::Human {
            text: human,
            read_only,
        }
    };
    let defaults = provenant_cli::Globals {
        json,
        state_dir: None,
        project: None,
    };
    let rendered = provenant_cli::render_or_publish(
        result,
        mode,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        |value| overflow::publish(value, globals.unwrap_or(&defaults)),
    );
    match rendered {
        Ok(status) => provenant_cli::exit_status(&status),
        Err(RenderError::TooLarge) => fail(
            &result.command,
            Outcome::Error,
            "RESULT_TOO_LARGE",
            "result exceeds 1 MiB; request a smaller page or read the owner-published artifact",
            json,
        ),
        Err(_) => {
            let _ = io::stderr().write_all(b"provenant: IO: cannot write command result\n");
            3
        }
    }
}
fn fail(command: &str, status: Outcome, code: &str, message: &str, json: bool) -> u8 {
    let mut result = CommandResult::new(command, status, ());
    result.error = Some(CommandError {
        code: code.into(),
        message: message.into(),
        cause: None,
        repair: None,
    });
    emit(&result, json, "", false)
}
fn unavailable(command: &str, owner: &'static str, json: bool) -> u8 {
    let error = provenant_cli::CliError::OwnerUnavailable { owner };
    fail(
        command,
        Outcome::Unavailable,
        "OWNER_UNAVAILABLE",
        &error.to_string(),
        json,
    )
}
fn run(inv: provenant_cli::Invocation) -> u8 {
    let json = inv.globals.json;
    match inv.command {
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
                Ok((data, human)) => emit_bound(
                    &CommandResult::new("root", Outcome::Ok, data),
                    json,
                    &human,
                    true,
                    Some(&inv.globals),
                ),
                Err(error) => fail("root", Outcome::Error, "ROOT", &error.to_string(), json),
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
                    false,
                ),
            }
        }
        Command::Help { .. } => fail(
            "help",
            Outcome::Invalid,
            "INVALID",
            "help must be resolved by the parser",
            json,
        ),
        Command::Config(provenant_cli::Config::Show) => unavailable("config show", "config", json),
        Command::Fabric(provenant_cli::Fabric::Mcp) => unavailable("fabric mcp", "fabric", json),
        Command::Install(_) => unavailable("install", "config", json),
        Command::Uninstall(_) => unavailable("uninstall", "config", json),
        Command::Route(route) => match route {
            provenant_cli::Route::List(_) => unavailable("route list", "routing", json),
            provenant_cli::Route::Resolve(_) => unavailable("route resolve", "routing", json),
        },
        Command::Dispatch(_) => unavailable("dispatch", "run", json),
        Command::Lanes(_) => unavailable("lanes", "run", json),
        Command::Output(_) => unavailable("output", "run", json),
        Command::Cancel(_) => unavailable("cancel", "run", json),
        Command::Resume(_) => unavailable("resume", "run", json),
        Command::Reconcile(_) => unavailable("reconcile", "run", json),
        Command::Telemetry(provenant_cli::Telemetry::Stats(_)) => {
            unavailable("telemetry stats", "telemetry", json)
        }
        Command::Quota(quota) => match quota {
            provenant_cli::Quota::Show(_) => unavailable("quota show", "quota", json),
            provenant_cli::Quota::Tick { .. } => unavailable("quota tick", "quota", json),
        },
        Command::Worktree(worktree) => match worktree {
            provenant_cli::Worktree::Create { .. } => {
                unavailable("worktree create", "confinement", json)
            }
            provenant_cli::Worktree::List => unavailable("worktree list", "confinement", json),
            provenant_cli::Worktree::Remove(_) => {
                unavailable("worktree remove", "confinement", json)
            }
        },
        Command::Check(check) => {
            let (name, owner, checkout) = match check {
                provenant_cli::Check::Config(_) => ("check config", "config", false),
                provenant_cli::Check::Install { .. } => ("check install", "config", false),
                provenant_cli::Check::Harness(_) => ("check harness", "cli checks", true),
                provenant_cli::Check::Release { .. } => ("check release", "cli checks", true),
                provenant_cli::Check::Writing { .. } => ("check writing", "workflow", false),
            };
            if checkout
                && let Err(error) = roots::selected_project(inv.globals.project.as_deref(), true)
            {
                return fail(name, Outcome::Invalid, "CHECKOUT", &error.to_string(), json);
            }
            unavailable(name, owner, json)
        }
        Command::Hook(hook) => match hook {
            provenant_cli::Hook::Claude(hook) => match hook {
                provenant_cli::ClaudeHook::PreToolUse => {
                    unavailable("hook claude pre-tool-use", "confinement hook", json)
                }
                provenant_cli::ClaudeHook::PreCompact => {
                    unavailable("hook claude pre-compact", "workflow hook", json)
                }
                provenant_cli::ClaudeHook::Compact => {
                    unavailable("hook claude compact", "workflow hook", json)
                }
            },
            provenant_cli::Hook::Git(provenant_cli::GitHook::PrePush { .. }) => {
                unavailable("hook git pre-push", "confinement hook", json)
            }
        },
    }
}
fn main() -> ExitCode {
    let status = match provenant_cli::parse(std::env::args_os().skip(1)) {
        ParseOutcome::Invocation(inv) => run(inv),
        ParseOutcome::Invalid { error, json } => fail(
            "parse",
            Outcome::Invalid,
            "INVALID",
            &error.to_string(),
            json,
        ),
        ParseOutcome::Help {
            text,
            catalog,
            json,
        } => emit(
            &CommandResult::new("help", Outcome::Ok, catalog),
            json,
            &text,
            true,
        ),
        ParseOutcome::Version { json } => match identity() {
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
                    json,
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
                json,
            ),
        },
    };
    ExitCode::from(status)
}
