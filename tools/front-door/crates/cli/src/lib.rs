//! Pure argument parsing, help and bounded result rendering. No owner execution.
#![forbid(unsafe_code)]
use provenant_core::edit_distance;
pub mod accounts;
pub mod bench;
pub mod check;
pub mod console;
pub mod domains;
pub mod fabric;
pub mod quota;
pub(crate) mod quota_history;
pub mod route;
pub mod run;
pub mod tracker;
pub mod wait;
pub mod workflow;
pub use bench::Bench;
pub use fabric::Fabric;
pub use quota::Quota;
pub use route::Route;
pub use workflow::Workflow;
mod grammar;
pub mod inbox;
pub mod install;
pub mod limits;
mod metadata;
pub mod mileage;
pub mod pr_body;
pub mod provider_updates;
mod render;
use clap::{Arg, ArgAction, ArgGroup, CommandFactory, FromArgMatches};
pub use grammar::*;
use provenant_records::{HelpArgument, HelpCatalogue, HelpCommand, HelpErrorCode, HelpGroup};
pub use render::*;
use std::ffi::OsString;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    Invalid(String),
    #[error("{owner} owner is unavailable in this build")]
    OwnerUnavailable { owner: &'static str },
}
#[derive(Debug)]
pub enum ParseOutcome {
    Invocation(Invocation),
    Help {
        text: String,
        catalog: HelpCatalogue,
        globals: Globals,
    },
    Version {
        globals: Globals,
    },
    Invalid {
        error: CliError,
        globals: Globals,
    },
}
/// Iterator contains arguments after argv[0]. Only the JSON transport bit is
/// pre-scanned; Clap owns option values, including recovery for help and errors.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> ParseOutcome {
    let mut args: Vec<_> = args.into_iter().collect();
    if args.is_empty() {
        args.push(OsString::from("help"));
    }
    let json = args
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json");
    let mut model = command_model();
    let mut argv = vec![OsString::from("provenant")];
    argv.extend(args);
    let matches = match model.try_get_matches_from_mut(&argv) {
        Ok(matches) => matches,
        Err(error) => {
            // Help/version short-circuit Clap. A passive, tolerant pass recovers
            // only values Clap accepted, without interpreting option text here.
            let recovered = build_model(true, true)
                .ignore_errors(true)
                .try_get_matches_from(&argv)
                .ok();
            let (globals, duplicate) = recovered
                .as_ref()
                .map(|matches| parsed_globals(matches, json))
                .unwrap_or((
                    Globals {
                        json,
                        ..Globals::default()
                    },
                    None,
                ));
            if let Some(error) = duplicate {
                return ParseOutcome::Invalid { error, globals };
            }
            return match error.kind() {
                clap::error::ErrorKind::DisplayHelp => match help_scope(&model, &argv) {
                    Some(scope) => ParseOutcome::Help {
                        text: if scope == "provenant" {
                            core_help(&model)
                        } else {
                            error.to_string()
                        },
                        catalog: scoped(
                            if scope == "provenant" {
                                catalog(&model)
                            } else {
                                catalog(&expanded_model())
                            },
                            &scope,
                        ),
                        globals,
                    },
                    None => ParseOutcome::Invalid {
                        error: CliError::Invalid("unknown help command".into()),
                        globals,
                    },
                },
                clap::error::ErrorKind::DisplayVersion => ParseOutcome::Version { globals },
                _ => ParseOutcome::Invalid {
                    error: clap_error(error, &argv),
                    globals,
                },
            };
        }
    };
    let (globals, duplicate) = parsed_globals(&matches, json);
    if let Some(error) = duplicate {
        return ParseOutcome::Invalid { error, globals };
    }
    let invocation = match RawInvocation::from_arg_matches(&matches) {
        Ok(value) => Invocation {
            globals,
            command: value.command,
            leaf: std::iter::successors(matches.subcommand(), |(_, sub)| sub.subcommand())
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
                .join(" "),
        },
        Err(error) => {
            return ParseOutcome::Invalid {
                error: clap_error(error, &argv),
                globals,
            };
        }
    };
    if let Command::Help { command, all } = &invocation.command {
        let mut expanded = expanded_model();
        let mut selected = &mut expanded;
        let mut scope = String::from("provenant");
        for component in command {
            selected = match selected.find_subcommand_mut(component) {
                Some(child) if !child.is_hide_set() => {
                    scope = format!("{scope} {}", child.get_name());
                    child
                }
                _ => {
                    return ParseOutcome::Invalid {
                        error: CliError::Invalid("unknown help command".into()),
                        globals: invocation.globals,
                    };
                }
            };
        }
        return ParseOutcome::Help {
            text: if command.is_empty() && !all {
                core_help(&model)
            } else {
                if command.is_empty() && *all {
                    format!(
                        "{}\nFor advanced options, run provenant help COMMAND.\n",
                        selected.render_long_help()
                    )
                } else {
                    selected.render_long_help().to_string()
                }
            },
            catalog: scoped(
                catalog(if command.is_empty() && !all {
                    &model
                } else {
                    &expanded
                }),
                &scope,
            ),
            globals: invocation.globals,
        };
    }
    match validate(&invocation) {
        Ok(()) => ParseOutcome::Invocation(invocation),
        Err(error) => ParseOutcome::Invalid {
            error,
            globals: invocation.globals,
        },
    }
}
fn clap_error(mut error: clap::Error, argv: &[OsString]) -> CliError {
    // Clap normally includes the rejected value. Fence tokens are secrets,
    // including malformed tokens, so redact them before either renderer.
    use clap::error::{ContextKind, ContextValue};
    let suggestion = usage_suggestion(&error, argv);
    if error
        .get(ContextKind::InvalidArg)
        .is_some_and(|arg| arg.to_string().starts_with("--token"))
    {
        error.insert(
            ContextKind::InvalidValue,
            ContextValue::String("[redacted]".into()),
        );
    }
    let mut text = error.to_string();
    if argv.iter().any(|arg| arg == "fabric") {
        // Clap may echo a secret as an unexpected positional, an invalid value,
        // a suggestion or a conflict. Scrub every run of 32 or more hex digits
        // in argv, including runs embedded in a longer value, in every error
        // kind before either renderer receives the diagnostic.
        for arg in argv {
            if let Some(arg) = arg.to_str() {
                for run in arg.split(|c: char| !c.is_ascii_hexdigit()) {
                    if run.len() >= 32 {
                        text = text.replace(run, "[redacted]");
                    }
                }
            }
        }
    }
    let message = text
        .split("\nUsage:")
        .next()
        .unwrap_or(&text)
        .split("\nFor more information")
        .next()
        .unwrap_or(&text)
        .split("\n  [subcommands:")
        .next()
        .unwrap_or(&text)
        .trim();
    let example = usage_example(argv);
    let message = message.strip_prefix("error: ").unwrap_or(message);
    let mut message = match example {
        Some(example) => format!("{message}\nexample: {example}"),
        None => message.into(),
    };
    if let Some(suggestion) = suggestion {
        message.push_str(&format!("\ndid you mean `{suggestion}`?"));
    }
    CliError::Invalid(message)
}
fn usage_suggestion(error: &clap::Error, argv: &[OsString]) -> Option<String> {
    use clap::error::ContextKind;
    if !matches!(
        error.kind(),
        clap::error::ErrorKind::UnknownArgument | clap::error::ErrorKind::InvalidSubcommand
    ) {
        return None;
    }
    let flag = error.get(ContextKind::InvalidArg);
    let rejected = flag
        .or_else(|| error.get(ContextKind::InvalidSubcommand))?
        .to_string();
    if rejected.len() > 80 || !rejected.is_ascii() {
        return None;
    }
    let model = expanded_model();
    let (selected, path) = walk_subcommands(&model, argv, Some(&rejected))?;
    let candidates: Vec<_> = if flag.is_some() {
        selected
            .get_arguments()
            .filter_map(|arg| arg.get_long().map(|name| format!("--{name}")))
            .collect()
    } else {
        selected
            .get_subcommands()
            .filter(|child| !child.is_hide_set())
            .map(|child| child.get_name().to_owned())
            .collect()
    };
    let candidate = candidates
        .into_iter()
        .map(|name| (edit_distance(&rejected, &name), name))
        .filter(|(distance, _)| (1..=2).contains(distance))
        .min()?;
    Some(if flag.is_some() {
        candidate.1
    } else {
        format!("{path} {}", candidate.1)
    })
}
/// Follow the subcommand words in `argv` (skipping option values) down the
/// command model, stopping at `stop` when given. Returns the deepest command
/// reached and its spelled path, such as `provenant pr land`.
fn walk_subcommands<'a>(
    model: &'a clap::Command,
    argv: &[OsString],
    stop: Option<&str>,
) -> Option<(&'a clap::Command, String)> {
    let mut selected = model;
    let mut path = String::from("provenant");
    let mut words = argv.iter().skip(1);
    while let Some(word) = words.next() {
        let word = word.to_str()?;
        if stop == Some(word) || word == "--" {
            break;
        }
        if let Some(option) = word.strip_prefix("--") {
            let (name, joined) = option
                .split_once('=')
                .map_or((option, false), |(name, _)| (name, true));
            if !joined
                && selected.get_arguments().any(|arg| {
                    arg.get_long() == Some(name)
                        && arg
                            .get_num_args()
                            .is_some_and(|range| range.min_values() > 0)
                })
            {
                words.next();
            }
        } else if let Some(child) = selected.find_subcommand(word) {
            selected = child;
            path.push(' ');
            path.push_str(child.get_name());
        }
    }
    Some((selected, path))
}
/// An example for the command the caller was invoking, chosen by the parsed
/// subcommand path rather than by words anywhere in argv.
fn usage_example(argv: &[OsString]) -> Option<String> {
    let model = expanded_model();
    let (_, mut path) = walk_subcommands(&model, argv, None)?;
    // `pr land` is retired; the walk stops at `pr`, so look at the word after it.
    let retired_pr_land = path == "provenant pr"
        && argv
            .iter()
            .skip_while(|arg| *arg != "pr")
            .nth(1)
            .is_some_and(|arg| arg == "land");
    if retired_pr_land {
        path = "provenant land".into();
    }
    let lands = matches!(path.as_str(), "provenant land" | "provenant pr land");
    if lands || path == "provenant pr status" {
        // `--wait` takes its duration only as `--wait=5m`; a spaced one is
        // read as a positional, so name the joined spelling.
        let spaced = argv
            .windows(2)
            .filter(|pair| pair[0] == "--wait")
            .filter_map(|pair| pair[1].to_str())
            .find(|value| {
                !value.starts_with('-')
                    && value.len() <= 8
                    && value.bytes().all(|b| b.is_ascii_alphanumeric())
                    && value.bytes().next().is_some_and(|b| b.is_ascii_digit())
            });
        if let Some(value) = spaced {
            let target = if lands { "land 2199" } else { "pr status 2199" };
            return Some(format!("provenant {target} --wait={value}"));
        }
    }
    match path.as_str() {
        "provenant land" | "provenant pr land" => Some("provenant land 2199 --wait".into()),
        "provenant route resolve" => Some(
            "provenant route resolve --model ADAPTER/ID (or provenant route resolve senior)".into(),
        ),
        "provenant output" => Some("provenant output ATTEMPT --host HOST".into()),
        "provenant run" => Some("provenant run 'Meet the acceptance criteria' --use task".into()),
        _ => None,
    }
}
fn parsed_globals(matches: &clap::ArgMatches, json: bool) -> (Globals, Option<CliError>) {
    fn collect(matches: &clap::ArgMatches, into: &mut RawGlobals) {
        if let Some(values) = matches.get_many::<bool>("json") {
            into.json.extend(values.copied());
        }
        if let Some(values) = matches.get_many::<bool>("full") {
            into.full.extend(values.copied());
        }
        if let Some(values) = matches.get_many::<std::path::PathBuf>("state_dir") {
            into.state_dir.extend(values.cloned());
        }
        if let Some(values) = matches.get_many::<std::path::PathBuf>("project") {
            into.project.extend(values.cloned());
        }
        if let Some((_, child)) = matches.subcommand() {
            collect(child, into);
        }
    }
    let mut raw = RawGlobals {
        json: Vec::new(),
        full: Vec::new(),
        state_dir: Vec::new(),
        project: Vec::new(),
    };
    collect(matches, &mut raw);
    let duplicate = (raw.json.len() > 1
        || raw.full.len() > 1
        || raw.state_dir.len() > 1
        || raw.project.len() > 1)
        .then(|| CliError::Invalid("global options may occur only once".into()));
    fn unique<T>(mut values: Vec<T>) -> Option<T> {
        if values.len() == 1 {
            values.pop()
        } else {
            None
        }
    }
    (
        Globals {
            json,
            full: !raw.full.is_empty(),
            state_dir: unique(raw.state_dir),
            project: unique(raw.project),
        },
        duplicate,
    )
}

pub fn command_model() -> clap::Command {
    build_model(false, false)
}
// The examples mark the commands included in the compact landing page; their
// descriptions come from the same command model as the parser and catalogue.
fn core_help(model: &clap::Command) -> String {
    let mut text = String::from("Provenant: run work, inspect progress and land changes.\n\n");
    let mut commands: Vec<_> = model
        .get_subcommands()
        .filter_map(|command| {
            command
                .get::<metadata::CoreCommand>()
                .map(|marker| (marker, command))
        })
        .collect();
    commands.sort_by_key(|(marker, _)| marker.0);
    for (marker, command) in commands {
        let name = command.get_name();
        let example = marker.1;
        let about = command
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default();
        text.push_str(&format!("  {name:<8}  {about}\n            {example}\n"));
    }
    text.push_str("  --full    Complete human detail; --json keeps machine output\n  help --all  All commands and advanced options\n              provenant help --all\n");
    text
}
fn expanded_model() -> clap::Command {
    build_model(false, true)
}
fn build_model(passive: bool, advanced: bool) -> clap::Command {
    fn configure(
        mut command: clap::Command,
        globals: &[Arg],
        parent: &str,
        advanced: bool,
    ) -> clap::Command {
        let path = if parent.is_empty() {
            command.get_name().to_owned()
        } else {
            format!("{parent} {}", command.get_name())
        };
        for global in globals {
            if command
                .get_arguments()
                .any(|arg| arg.get_id() == global.get_id())
            {
                command = command.mut_arg(global.get_id().clone(), |arg| arg.global(false));
            } else {
                command = command.arg(global.clone());
            }
        }
        command = command
            .disable_help_flag(true)
            .disable_version_flag(true)
            .version(env!("CARGO_PKG_VERSION"));
        if command.get_name() == "resolve" {
            command = command.group(
                ArgGroup::new("route_choice")
                    .args(["name", "model", "route"])
                    .required(true),
            );
        }
        if !advanced && matches!(path.as_str(), "provenant retry") {
            let visible = [
                "full",
                "json",
                "host",
                "account",
                "prompt_file",
                "prompt_stdin",
                "brief",
                "node",
                "extend",
                "tier",
                "over",
                "file",
                "preset",
                "preset_set",
                "preset_params_file",
                "preset_plan",
                "fresh",
                "explain",
                "route",
                "model",
                "effort",
                "mode",
                "network",
                "worktree",
                "branch",
                "base",
                "wait",
                "all",
                "id",
            ];
            let advanced: Vec<_> = command
                .get_arguments()
                .filter(|arg| !arg.is_positional() && !visible.contains(&arg.get_id().as_str()))
                .map(|arg| arg.get_id().clone())
                .collect();
            for id in advanced {
                command = command.mut_arg(id, |arg| arg.hide(true));
            }
        }
        if !advanced
            && parent == "provenant"
            && command.get::<metadata::CoreCommand>().is_none()
            && command.get_name() != "help"
        {
            command = command.hide(true);
        }
        if path == "provenant worktree remove" {
            command = command.group(
                ArgGroup::new("remove_scope")
                    .args(["branch", "since"])
                    .multiple(false),
            );
        }
        if advanced {
            let ids: Vec<_> = command
                .get_arguments()
                .map(|arg| arg.get_id().clone())
                .collect();
            for id in ids {
                command = command.mut_arg(id, |arg| arg.hide(false));
            }
        }
        let children: Vec<_> = command
            .get_subcommands()
            .cloned()
            .map(|child| configure(child, globals, &path, advanced))
            .collect();
        for child in children {
            let name = child.get_name();
            command = command.mut_subcommand(name, |_| child.clone());
        }
        command
    }
    let base = RawInvocation::command();
    let globals: Vec<_> = base
        .get_arguments()
        .filter(|arg| {
            matches!(
                arg.get_id().as_str(),
                "json" | "full" | "state_dir" | "project"
            )
        })
        .cloned()
        .map(|arg| arg.global(false))
        .collect();
    let mut command = configure(base, &globals, "", advanced)
        .arg(
            Arg::new("help")
                .long("help")
                .help("Print help")
                .global(true)
                .action(if passive {
                    ArgAction::SetTrue
                } else {
                    ArgAction::HelpLong
                }),
        )
        .arg(
            Arg::new("version")
                .long("version")
                .help("Print version")
                .global(true)
                .action(if passive {
                    ArgAction::SetTrue
                } else {
                    ArgAction::Version
                }),
        )
        .version(env!("CARGO_PKG_VERSION"));
    command.build();
    command
}
fn invalid(message: &str) -> Result<(), CliError> {
    Err(CliError::Invalid(message.into()))
}
fn route_valid(route: &RouteRequest) -> Result<(), CliError> {
    if route.fallback.len() > 2
        || (route.fallback.len() == 2 && route.fallback.first() == route.fallback.last())
    {
        return invalid("fallback requires at most two distinct models");
    }
    Ok(())
}
fn validate(inv: &Invocation) -> Result<(), CliError> {
    if inv.globals.json
        && matches!(
            inv.command,
            Command::Hook(_) | Command::Internal(_) | Command::Fabric(Fabric::Mcp { .. })
        )
    {
        return invalid("--json is invalid for hook/MCP/internal execution");
    }
    match &inv.command {
        Command::Route(Route::Resolve(resolve)) => {
            if resolve.name.is_none()
                && resolve.request.model.is_none()
                && resolve.request.route.is_none()
            {
                return invalid(
                    "route resolve needs a route or model, such as `route resolve utility`, \
                     `route resolve codex/gpt-6-luna`, `--route utility` or `--model ADAPTER/ID`",
                );
            }
            route_valid(&resolve.request)?
        }
        Command::Reconcile(reconcile) if reconcile.id.len() > 100 => {
            return invalid("at most 100 IDs are allowed");
        }
        Command::Worktree(Worktree::Remove(remove)) if remove.branch.len() > 100 => {
            return invalid("at most 100 branches are allowed");
        }
        Command::Telemetry(Telemetry::Stats(stats)) if matches!((stats.since,stats.until),(Some(since),Some(until)) if since >= until) =>
        {
            return invalid("since must precede until");
        }
        _ => {}
    }
    Ok(())
}
/// The subcommand path named before the first `--help`, such as
/// `provenant fabric mail`. Flags are skipped, along with the values of the
/// global `--state-dir` and `--project`. A hidden command is unknown.
fn help_scope(model: &clap::Command, argv: &[OsString]) -> Option<String> {
    let mut scope = String::from("provenant");
    let mut current = model;
    let mut words = argv
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--" && *arg != "--help");
    while let Some(arg) = words.next() {
        if arg == "--state-dir" || arg == "--project" {
            words.next();
        } else if let Some(child) = arg.to_str().and_then(|name| current.find_subcommand(name)) {
            if child.get_name() == "internal" || (child.is_hide_set() && scope != "provenant") {
                return None;
            }
            scope = format!("{scope} {}", child.get_name());
            current = child;
        }
    }
    Some(scope)
}
/// Keep the named command and its descendants; `help` alone keeps all,
/// including the error codes.
fn scoped(mut catalogue: HelpCatalogue, scope: &str) -> HelpCatalogue {
    if scope != "provenant" {
        catalogue.error_codes.clear();
        catalogue.provider_controls.clear();
    }
    catalogue.commands.retain(|command| {
        command
            .command
            .strip_prefix(scope)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    });
    catalogue
}
fn catalog(model: &clap::Command) -> HelpCatalogue {
    fn walk(command: &clap::Command, path: String, commands: &mut Vec<HelpCommand>) {
        if command.is_hide_set() {
            return;
        }
        let arguments = command
            .get_arguments()
            .filter(|arg| !arg.is_hide_set())
            .map(|arg| {
                let name = arg.get_id().to_string();
                HelpArgument {
                    description: arg.get_help().map(ToString::to_string),
                    bounds: arg
                        .get::<metadata::NumericBounds>()
                        .map(|bounds| bounds.0)
                        .or_else(|| {
                            arg.get::<metadata::PublishedBounds>()
                                .map(|bounds| bounds.0)
                        })
                        .or_else(|| {
                            arg.get_num_args()
                                .filter(|range| {
                                    range.max_values() > 1 && range.max_values() < usize::MAX
                                })
                                .map(|range| (range.min_values() as u64, range.max_values() as u64))
                        }),
                    conditional_defaults: if name == "provider_timeout" {
                        vec![
                            ("read-only".into(), "3600".into()),
                            ("worktree-write".into(), "10800".into()),
                        ]
                    } else {
                        Vec::new()
                    },
                    requires: arg
                        .get::<metadata::Requirements>()
                        .map(|value| value.0.clone())
                        .unwrap_or_default(),
                    name,
                    long: arg.get_long().map(str::to_owned),
                    positional: arg.is_positional(),
                    required: arg.is_required_set(),
                    repeatable: !matches!(
                        arg.get_id().as_str(),
                        "json" | "full" | "state_dir" | "project"
                    ) && (matches!(arg.get_action(), ArgAction::Append)
                        || arg
                            .get_num_args()
                            .is_some_and(|range| range.max_values() > 1)),
                    defaults: if path == "provenant chair log append"
                        && arg.get_id().as_str() == "operation_id"
                    {
                        Vec::new()
                    } else {
                        arg.get_default_values()
                            .iter()
                            .map(|value| value.to_string_lossy().into_owned())
                            .collect()
                    },
                    values: arg
                        .get_value_parser()
                        .possible_values()
                        .map(|values| values.map(|v| v.get_name().to_owned()).collect())
                        .unwrap_or_default(),
                    conflicts: command
                        .get_arg_conflicts_with(arg)
                        .iter()
                        .map(|a| a.get_id().to_string())
                        .collect(),
                }
            })
            .collect();
        let groups = command
            .get_groups()
            .map(|group| HelpGroup {
                name: group.get_id().to_string(),
                arguments: group.get_args().map(ToString::to_string).collect(),
                required: group.is_required_set(),
                multiple: group.clone().is_multiple(),
            })
            .collect();
        commands.push(HelpCommand {
            command: path.clone(),
            description: command.get_about().map(ToString::to_string),
            arguments,
            groups,
            constraints: constraints(&path),
        });
        for child in command.get_subcommands() {
            walk(child, format!("{path} {}", child.get_name()), commands);
        }
    }
    let mut commands = Vec::new();
    walk(model, "provenant".into(), &mut commands);
    let error_codes = provenant_run::ErrorKind::ALL
        .iter()
        .map(|kind| HelpErrorCode {
            code: kind.code().into(),
            summary: kind.summary().into(),
        })
        .collect();
    HelpCatalogue {
        commands,
        error_codes,
        provider_controls: provenant_records::control::control_capabilities(),
    }
}
/// Canonical installed verb paths. Never derived from raw caller arguments.
pub fn usage_verbs() -> Vec<String> {
    fn walk(command: &clap::Command, path: String, verbs: &mut Vec<String>) {
        let children: Vec<_> = command.get_subcommands().collect();
        if children.is_empty() && !path.is_empty() {
            verbs.push(path);
        } else {
            // A parent whose subcommand is optional is also a verb (`hosts`).
            if !path.is_empty() && !command.is_subcommand_required_set() {
                verbs.push(path.clone());
            }
            for child in children {
                let name = child.get_name();
                walk(
                    child,
                    if path.is_empty() {
                        name.into()
                    } else {
                        format!("{path} {name}")
                    },
                    verbs,
                );
            }
        }
    }
    let mut verbs = vec!["parse".into(), "version".into()];
    walk(&command_model(), String::new(), &mut verbs);
    verbs
}
fn constraints(path: &str) -> Vec<String> {
    if path == "provenant output" {
        return vec![
            "target is an ATTEMPT ID, RUN/ATTEMPT, HOST/ATTEMPT or RUN/TASK/ATTEMPT".into(),
            format!("tail above {0} lines is clamped to {0}", limits::TAIL_LINES),
            format!(
                "a result shows whole up to {} KiB, then its head and tail; at most {} changed paths of {} bytes are named; missing evidence is null",
                limits::OUTPUT_BYTES / 1024,
                limits::OUTPUT_PATHS,
                limits::PATH_BYTES
            ),
            "peer tail scans share a 30-second deadline and reject changing artifacts".into(),
        ];
    }
    let values: &[&str] = match path {
        "provenant run" => &[
            "a brief uses task by default; --use selects a layered template",
            "--set binds template parameters using TOML values",
            "--file accepts a graph document or a plain text brief",
            "inside a lane, work joins the calling node's graph as a child",
        ],
        "provenant route resolve" => &[
            "exactly one of NAME, model or route",
            "fallback has at most two distinct models",
        ],
        "provenant telemetry stats" => &[
            "dates mean midnight UTC; timestamps need an offset",
            "since must precede until",
        ],
        "provenant retry" => &[
            "target is an ATTEMPT ID, RUN/ATTEMPT or RUN/TASK/ATTEMPT",
            "prompt is at most 1 MiB; expected generation is 0",
            "operation-id is generated when omitted; supply it for planned retries",
        ],
        "provenant chair log append" => &[
            "operation-id and issued-at are generated when omitted; supply both for planned retries",
        ],
        "provenant land" => &[
            "checks once by default; wait without a value means one hour; use --wait=30m for a duration",
            "required checks must register and pass on the same head; head changes refuse merging",
            "already-merged PRs retry safe cleanup without CI or another merge",
        ],
        "provenant status" => &[
            "wait requires at least one target via ID or repeated --target, up to domains.wait.max_targets (default 64); no duration uses domains.wait.timeout_seconds",
            "with --wait, --json emits newline-delimited snapshot/transition events and a final end event; timeout exits 124",
            "follow waits for all targets to be terminal; land targets only observe their PR",
        ],
        "provenant pr status" => &[
            "one head-and-checks snapshot per PR; unavailable PRs preserve other rows",
            "wait without a value means five minutes; unchanged snapshots exit 124",
        ],
        "provenant hook" | "provenant internal" => &["json transport is invalid for execution"],
        _ => &[],
    };
    values.iter().map(|value| (*value).into()).collect()
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    #[derive(clap::Parser)]
    struct DispatchFixture {
        #[command(flatten)]
        request: Dispatch,
    }
    fn dispatch_fixture(words: &[&str]) -> Dispatch {
        use clap::Parser;
        DispatchFixture::try_parse_from(words).unwrap().request
    }

    #[test]
    fn catalogue_numeric_bounds_match_parser_ranges() {
        let model = expanded_model();
        for entry in catalog(&model).commands {
            let command = entry
                .command
                .split_whitespace()
                .skip(1)
                .fold(&model, |command, name| {
                    command.find_subcommand(name).unwrap()
                });
            for argument in entry.arguments {
                let arg = command
                    .get_arguments()
                    .find(|arg| arg.get_id().as_str() == argument.name)
                    .unwrap();
                let Some(bounds) = arg.get::<metadata::NumericBounds>() else {
                    continue;
                };
                let (min, max) = bounds.0;
                assert_eq!(
                    argument.bounds,
                    Some((min, max)),
                    "{} {}",
                    entry.command,
                    argument.name
                );
                let accepts = |value: u64| {
                    clap::Command::new("probe")
                        .arg(Arg::new("value").value_parser(arg.get_value_parser().clone()))
                        .try_get_matches_from(["probe", &value.to_string()])
                        .is_ok()
                };
                assert!(
                    accepts(min),
                    "{} {} minimum {min}",
                    entry.command,
                    argument.name
                );
                assert!(
                    accepts(max),
                    "{} {} maximum {max}",
                    entry.command,
                    argument.name
                );
                if let Some(below) = min.checked_sub(1) {
                    assert!(!accepts(below));
                }
                if let Some(above) = max.checked_add(1) {
                    assert!(!accepts(above));
                }
            }
        }
        for (path, expected) in [
            ("provenant config history", (1, 500)),
            ("provenant quota history", (1, 100_000)),
        ] {
            let catalogue = catalog(&model);
            let command = catalogue
                .commands
                .iter()
                .find(|entry| entry.command == path)
                .unwrap();
            assert_eq!(
                command
                    .arguments
                    .iter()
                    .find(|arg| arg.name == "limit")
                    .unwrap()
                    .bounds,
                Some(expected)
            );
        }
    }
    #[test]
    fn published_non_numeric_bounds_and_weight_are_preserved() {
        let catalogue = catalog(&expanded_model());
        for (path, name, bounds) in [
            ("provenant stop", "reason", (0, 256)),
            ("provenant route set", "weight", (0, 100)),
        ] {
            let command = catalogue
                .commands
                .iter()
                .find(|c| c.command == path)
                .unwrap();
            assert_eq!(
                command
                    .arguments
                    .iter()
                    .find(|a| a.name == name)
                    .unwrap()
                    .bounds,
                Some(bounds),
                "{path} {name}"
            );
        }
        invalid(&["stop", "graph", "--reason", &"é".repeat(129)]);
        valid(&["stop", "graph", "--reason", &"é".repeat(128)]);
    }
    #[test]
    fn ranged_parsers_use_catalogue_builders() {
        fn check(dir: &std::path::Path) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    check(&path);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && path.file_name().unwrap() != "metadata.rs"
                {
                    let text = std::fs::read_to_string(&path).unwrap();
                    assert!(
                        !text.contains(concat!(".ran", "ge(")),
                        "{} bypasses catalogue range builders",
                        path.display()
                    );
                }
            }
        }
        check(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    }
    #[test]
    fn compact_help_uses_marked_commands_and_their_descriptions() {
        let model = command_model();
        let text = core_help(&model);
        for command in model.get_subcommands() {
            if let Some(marker) = command.get::<metadata::CoreCommand>() {
                assert!(!command.is_hide_set());
                assert!(text.contains(&command.get_about().unwrap().to_string()));
                assert!(text.contains(marker.1));
            }
        }
    }
    #[test]
    fn catalogue_paths_exist_in_command_model() {
        let model = expanded_model();
        for entry in catalog(&model).commands {
            let mut words = entry.command.split_whitespace();
            assert_eq!(words.next(), Some(model.get_name()));
            let mut current = &model;
            for word in words {
                current = current
                    .find_subcommand(word)
                    .unwrap_or_else(|| panic!("missing {}", entry.command));
            }
        }
    }
    #[test]
    fn config_check_reuses_the_check_config_page_arguments() {
        let config = valid(&["config", "check", "--limit", "75", "--cursor", "cursor"]);
        let check = valid(&["check", "config", "--limit", "75", "--cursor", "cursor"]);
        let Command::Config(Config::Check(config)) = config.command else {
            panic!("config check");
        };
        let Command::Check(Check::Config(check)) = check.command else {
            panic!("check config");
        };
        assert_eq!(config.limit, check.limit);
        assert_eq!(config.cursor, check.cursor);
        invalid(&["config", "check", "--limit", "0"]);
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "config", "check", "--json"])
        else {
            panic!("config check help");
        };
        assert!(
            catalog
                .commands
                .iter()
                .any(|command| command.command == "provenant config check")
        );
    }
    #[test]
    fn unified_lifecycle_commands_accept_graph_node_and_attempt_ids() {
        for id in [
            "graph-build",
            "graph-build/verify",
            "verify",
            "a",
            "face",
            "DEADBEEF",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "11111111111111111111111111111111",
        ] {
            valid(&["status", id, "--wait", "--json"]);
            valid(&["output", id]);
            valid(&["steer", id, "--text-stdin"]);
            valid(&["stop", id]);
            valid(&["retry", id]);
            let Command::Retry(retry) = valid(&[
                "retry",
                id,
                "--node",
                "verify",
                "--brief",
                "Repair the failure",
                "--extend",
                "budget=20",
            ])
            .command
            else {
                panic!("retry command");
            };
            assert_eq!(retry.node.as_deref(), Some("verify"));
            assert_eq!(retry.brief.as_deref(), Some("Repair the failure"));
            assert_eq!(retry.extend, ["budget=20"]);
        }
        let long_node = "n".repeat(200);
        valid(&["output", &long_node]);
        let Command::Stop(stop) = valid(&["stop", "DEADBEEF"]).command else {
            panic!("stop");
        };
        assert_eq!(stop.target.selection.as_deref(), Some("DEADBEEF"));
        for words in [
            &["lanes"][..],
            &["resume", "graph-build"],
            &["continue", "graph-build"],
            &["cancel", "graph-build"],
            &["redo", "graph-build"],
            &["fabric", "graph", "status", "graph-build"],
        ] {
            invalid(words);
        }
    }

    #[test]
    fn core_path_help_and_defaults() {
        let expected = [
            "run",
            "status",
            "output",
            "steer",
            "stop",
            "retry",
            "edit",
            "templates",
            "land",
            "quota",
        ];
        for words in [&[][..], &["help"][..]] {
            let ParseOutcome::Help { text, catalog, .. } = parsed(words) else {
                panic!("core help")
            };
            let actual = catalog
                .commands
                .iter()
                .map(|entry| entry.command.strip_prefix("provenant "))
                .flatten()
                .filter(|path| !path.contains(' ') && *path != "help")
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(actual, expected.into_iter().collect());
            assert!(text.contains("help --all"));
        }
        for command in ["dispatch", "review", "lanes", "resume", "cancel", "preset"] {
            invalid(&[command]);
        }
        invalid(&["fabric", "graph", "status", "example"]);
        valid(&["status"]);
        valid(&["land", "1947"]);
        valid(&["quota"]);
        assert!(matches!(
            parsed(&["help", "--all"]),
            ParseOutcome::Help { .. }
        ));
    }

    #[test]
    fn graph_run_and_template_controls_parse() {
        let Command::Run(request) = valid(&[
            "run",
            "Inspect the code",
            "--use",
            "explore",
            "--tier",
            "junior",
            "--over",
            "cli",
            "--over",
            "runtime",
            "--set",
            "limit=3",
            "--explain",
            "--wait=30m",
        ])
        .command
        else {
            panic!("run")
        };
        assert_eq!(request.brief.as_deref(), Some("Inspect the code"));
        assert_eq!(request.template.as_deref(), Some("explore"));
        assert_eq!(request.tier.as_deref(), Some("junior"));
        assert_eq!(request.over, ["cli", "runtime"]);
        assert_eq!(request.set, ["limit=3"]);
        assert!(request.explain);
        assert_eq!(request.wait, Some(1800));
        valid(&["run", "--file", "work.toml", "--explain"]);
        invalid(&["run", "--preset", "task"]);
        valid(&["stats", "--by", "tier"]);
        invalid(&["stats", "--by", "node"]);
        valid(&["status", "--stats", "--by", "tier"]);
        valid(&["status", "--stats", "--by", "template"]);
        invalid(&["status", "--by", "tier"]);
        invalid(&["status", "--stats", "--by", "node"]);
        valid(&["templates"]);
        valid(&["templates", "review.security", "--file", "inline.toml"]);
        valid(&["edit", "example", "--file", "patch.toml"]);
        invalid(&["edit", "example"]);
        for flag in [
            "--preset",
            "--prompt-file",
            "--model",
            "--route",
            "--manifest",
            "--plan",
        ] {
            invalid(&["run", flag, "example"]);
        }
    }
    #[test]
    fn expanded_help_catalogue_describes_commands_and_requirements() {
        let ParseOutcome::Help { text, catalog, .. } = parsed(&["help", "--all"]) else {
            panic!("help");
        };
        assert!(text.contains("provenant help COMMAND"));
        let run = catalog
            .commands
            .iter()
            .find(|command| command.command == "provenant run")
            .unwrap();
        assert!(
            run.description
                .as_deref()
                .is_some_and(|text| !text.is_empty())
        );
        for name in [
            "brief", "template", "tier", "over", "set", "file", "explain", "wait",
        ] {
            assert!(
                run.arguments.iter().any(|argument| argument.name == name),
                "{name}"
            );
        }
        let status = catalog
            .commands
            .iter()
            .find(|c| c.command == "provenant status")
            .unwrap();
        for name in ["target", "until", "follow"] {
            assert_eq!(
                status
                    .arguments
                    .iter()
                    .find(|a| a.name == name)
                    .unwrap()
                    .requires,
                ["wait"]
            );
        }
        assert!(
            status
                .constraints
                .iter()
                .any(|c| c.contains("newline-delimited"))
        );
    }
    #[test]
    fn chair_log_help_explains_generated_retry_keys() {
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "--all"]) else {
            panic!("catalog");
        };
        let Some(command) = catalog
            .commands
            .iter()
            .find(|c| c.command == "provenant chair log append")
        else {
            panic!("chair log append help");
        };
        assert!(
            command
                .constraints
                .iter()
                .any(|c| c.contains("supply both for planned retries"))
        );
        let Some(operation) = command.arguments.iter().find(|a| a.name == "operation_id") else {
            panic!("operation ID argument");
        };
        assert!(operation.defaults.is_empty());
        assert_eq!(operation.requires, ["issued_at"]);
        let Some(time) = command.arguments.iter().find(|a| a.name == "issued_at") else {
            panic!("issued at argument");
        };
        assert_eq!(time.requires, ["operation_id"]);
        valid(&[
            "chair",
            "log",
            "append",
            "--session",
            "chair",
            "--file",
            "input",
        ]);
        valid(&[
            "chair",
            "log",
            "append",
            "--session",
            "chair",
            "--file",
            "input",
            "--operation-id",
            "op",
            "--issued-at",
            "123",
        ]);
        invalid(&[
            "chair",
            "log",
            "append",
            "--session",
            "chair",
            "--file",
            "input",
            "--operation-id",
            "op",
        ]);
        invalid(&[
            "chair",
            "log",
            "append",
            "--session",
            "chair",
            "--file",
            "input",
            "--issued-at",
            "123",
        ]);
    }
    #[test]
    fn land_accepts_a_pinned_head_and_hides_it_from_default_help() {
        for length in [40, 64] {
            let sha = "a".repeat(length);
            let inv = valid(&["land", "1953", "--head", &sha]);
            let Command::Land { numbers, head, .. } = inv.command else {
                panic!("land command");
            };
            assert_eq!(numbers, [1953]);
            assert_eq!(head.map(|head| head.to_string()), Some(sha));
        }
        invalid(&["land", "1953", "--head", "abc"]);
        let mut model = command_model();
        let Some(land) = model.find_subcommand_mut("land") else {
            panic!("land grammar");
        };
        assert!(!land.render_help().to_string().contains("--head"));
        let ParseOutcome::Help { text, .. } = parsed(&["help", "land"]) else {
            panic!("advanced land help");
        };
        assert!(text.contains("--head"), "{text}");
    }
    #[test]
    fn batch_land_and_policy_explanation_parse() -> Result<(), Box<dyn std::error::Error>> {
        let inv = valid(&[
            "land", "12", "34", "--batch", "--shadow", "--tier", "crucial",
        ]);
        let Command::Land {
            numbers,
            batch,
            shadow,
            tier,
            ..
        } = inv.command
        else {
            return Err("expected land command".into());
        };
        assert_eq!(numbers, [12, 34]);
        assert!(batch && shadow);
        assert_eq!(tier.as_deref(), Some("crucial"));
        invalid(&["land", "12", "--tier", "high"]);
        valid(&["land", "--explain", "--json"]);
        invalid(&["land", "0", "--batch"]);
        Ok(())
    }

    #[test]
    fn agent_output_controls_and_land_wait_are_global_and_bounded() {
        valid(&["--full", "quota"]);
        valid(&["route", "list", "--full"]);
        valid(&["land", "2199", "--wait"]);
        valid(&["land", "--wait=30m", "2199"]);
        invalid(&["land", "2199", "--wait=0"]);
        invalid(&["land", "2199", "--wait=5h"]);
        let attempt = "11".repeat(16);
        valid(&["output", "--host", "workshop", &attempt, "--json"]);
        invalid(&["output", &attempt, "--summary"]);
    }
    #[test]
    fn common_usage_errors_name_the_correct_invocation() {
        let ParseOutcome::Invalid { error, .. } = parsed(&["status", "--wait=5h"]) else {
            panic!("expected duration error");
        };
        assert!(error.to_string().contains("--wait"), "{error}");
        for (args, flag) in [(vec!["quota", "history", "--until", "0d"], "--until")] {
            let ParseOutcome::Invalid { error, .. } = parsed(&args) else {
                panic!("expected span error");
            };
            assert!(error.to_string().contains(flag), "{error}");
        }
        for (words, example) in [
            (
                vec!["route", "resolve", "one", "two"],
                "provenant route resolve --model ADAPTER/ID",
            ),
            (
                vec!["output", "--host"],
                "provenant output ATTEMPT --host HOST",
            ),
            (
                vec!["land", "2199", "--wait", "5m"],
                "provenant land 2199 --wait=5m",
            ),
            (vec!["pr", "land", "2199"], "provenant land 2199 --wait"),
        ] {
            let ParseOutcome::Invalid { error, .. } = parsed(&words) else {
                panic!("expected usage error");
            };
            assert!(error.to_string().contains(example), "{error}");
        }
    }
    #[test]
    fn misspelt_subcommands_and_flags_suggest_the_scoped_form() {
        for (words, suggestion) in [
            (vec!["statsu"], "provenant status"),
            (vec!["pr", "statsu"], "provenant pr status"),
            (vec!["quota", "--fullx"], "--full"),
        ] {
            let ParseOutcome::Invalid { error, .. } = parsed(&words) else {
                panic!("usage error");
            };
            assert!(error.to_string().contains("did you mean"), "{error}");
            assert!(error.to_string().contains(suggestion), "{error}");
        }
    }
    fn parsed(words: &[&str]) -> ParseOutcome {
        parse(words.iter().map(OsString::from))
    }
    fn valid(words: &[&str]) -> Invocation {
        match parsed(words) {
            ParseOutcome::Invocation(inv) => inv,
            other => panic!("{words:?}: {other:?}"),
        }
    }
    fn invalid(words: &[&str]) {
        assert!(
            matches!(parsed(words), ParseOutcome::Invalid { .. }),
            "{words:?}"
        );
    }
    #[test]
    fn config_migrate_takes_dry_run() {
        for (words, expected) in [
            (vec!["config", "migrate"], false),
            (vec!["config", "migrate", "--dry-run"], true),
        ] {
            let Command::Config(Config::Migrate { dry_run }) = valid(&words).command else {
                panic!("config migrate")
            };
            assert_eq!(dry_run, expected);
        }
    }

    #[test]
    fn writing_paths_and_private_descriptors_are_bounded() {
        let mut words = vec!["check", "writing"];
        words.extend(std::iter::repeat_n("fixture.md", 256));
        valid(&words);
        words.push("overflow.md");
        invalid(&words);
        valid(&["internal", "supervise"]);
        valid(&["internal", "supervise", "3", "4", "5", "6", "7"]);
        invalid(&["internal", "supervise", "3"]);
        invalid(&["internal", "supervise", "2", "4", "5", "6", "7"]);
    }
    #[test]
    fn bounded_waits_counts_and_byte_windows() {
        valid(&[
            "status", "--wait", "--target", "pr:2721", "--until", "all", "--follow",
        ]);
        valid(&[
            "status",
            "--wait=2m",
            "--target",
            "graph:build",
            "--target",
            "pr:2721",
        ]);
        invalid(&["status", "--wait=0"]);
        invalid(&["status", "--wait=5h"]);
        invalid(&["status", "--follow"]);
        invalid(&["status", "--target", "pr:2721"]);
        for wait in ["1", "14400"] {
            valid(&["status", &format!("--wait={wait}")]);
        }
        for wait in ["0", "14401", "-1", "1.5", "18446744073709551616"] {
            invalid(&["status", &format!("--wait={wait}")]);
        }
        valid(&["status", "--limit", "100"]);
        invalid(&["status", "--limit", "101"]);
        let attempt = "11".repeat(16) + &format!("/{}/{}", "22".repeat(16), "33".repeat(16));
        valid(&["output", &attempt, "--tail", "500", "--max-bytes", "65536"]);
        valid(&["output", &attempt, "--tail", "501"]);
        invalid(&["output", &attempt, "--tail", "0"]);
        invalid(&["output", &attempt, "--max-bytes", "65537"]);
        invalid(&["output", &attempt, "--offset", "0", "--tail", "1"]);
        valid(&["output", &attempt, "--trace"]);
        valid(&["telemetry", "trace", "--peers", "--run", &"11".repeat(16)]);
        invalid(&["output", &attempt, "--trace", "--part", "prompt"]);
        invalid(&["output", &attempt, "--trace", "--part", "receipt"]);
        invalid(&["output", &attempt, "--trace", "--tail", "1"]);
        valid(&["output", &"11".repeat(16)]);
        valid(&["output", &"11".repeat(16), "--host", "workshop"]);
        valid(&[
            "output",
            &format!("{}/{}", "11".repeat(16), "33".repeat(16)),
        ]);
        invalid(&["output", &format!("{}/", "11".repeat(16))]);
    }
    #[test]
    fn session_handoff_names_a_peer_and_an_account() {
        valid(&[
            "workflow",
            "session",
            "handoff",
            "--to",
            "workshop",
            "--account",
            "work",
        ]);
        valid(&[
            "workflow",
            "session",
            "handoff",
            "--to",
            "workshop",
            "--account",
            "work",
            "--note",
            "one",
            "--note",
            "two",
            "--attach",
            "a.md",
            "--prompt-file",
            "p.md",
            "--no-checkpoint",
            "--model",
            "opus",
            "--wip",
            "--dry-run",
            "--no-launch",
            "--session-id",
            "explicit",
        ]);
        invalid(&["workflow", "session", "handoff", "--to", "workshop"]);
        invalid(&[
            "workflow",
            "session",
            "handoff",
            "--to",
            "w",
            "--account",
            "a",
            "--wip",
            "--allow-dirty",
        ]);
    }
    #[test]
    fn disk_report_accepts_globals_and_has_no_mutation_flags() {
        valid(&["disk", "report"]);
        valid(&[
            "--json",
            "--state-dir",
            "/fixture/state",
            "disk",
            "report",
            "--project",
            "/fixture/repo",
        ]);
        let ParseOutcome::Invocation(invocation) = parsed(&["disk", "report", "--json"]) else {
            panic!("disk report should parse")
        };
        assert_eq!(invocation.leaf, "disk report");
        assert!(matches!(invocation.command, Command::Disk(Disk::Report)));
        invalid(&["disk"]);
        invalid(&["disk", "delete"]);
        invalid(&["disk", "report", "--force"]);
        invalid(&["disk", "report", "--dry-run"]);
    }
    #[test]
    fn run_refuses_adapter_unsupported_flags_before_side_effects() {
        // Models are catalogued, so an effort is refused only by the adapter
        // (cursor) or by the catalogue entry, which records no effort (opencode).
        for (model, effort, refused) in [
            ("claude/claude-sonnet-5-5", "high", false),
            ("codex/gpt-6-luna", "high", false),
            ("agy/gemini-3.8-flash", "high", false),
            ("opencode/opencode-go/glm-5.3-flash", "high", true),
            ("cursor/gpt-5.3-codex-low", "high", true),
        ] {
            let request = dispatch_fixture(&[
                "fixture",
                "--prompt-stdin",
                "--model",
                model,
                "--effort",
                effort,
                "--worktree-branch",
                "feat/x",
            ]);
            let result = run::check_flag_capabilities(&request, None);
            assert_eq!(result.is_err(), refused, "{model}");
            if let Err(fail) = result {
                assert_eq!(fail.kind.code(), "UNSUPPORTED", "{model}");
            }
        }
    }
    #[test]
    fn manifest_task_is_refused_before_any_launch_side_effect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let write = |model: &str| {
            std::fs::write(
                &path,
                format!(
                    r#"{{"schema_version":1,"tasks":[{{"prompt":"p","route":{{"kind":"model","provider":"cursor","model":"{model}","effort":"high"}}}}]}}"#
                ),
            )
            .unwrap();
        };
        write("cursor/m");
        let request = dispatch_fixture(&["fixture", "--manifest", path.to_str().unwrap()]);
        let fail = run::check_flag_capabilities(&request, None).unwrap_err();
        assert_eq!(fail.kind.code(), "UNSUPPORTED");
    }
    #[test]
    fn pool_effort_is_refused_only_when_no_member_takes_it() {
        assert!(run::pool_effort_refusal(&["cursor", "codex"], "high").is_none());
        assert!(run::pool_effort_refusal(&["cursor"], "high").is_some());
        assert!(run::pool_effort_refusal(&["cursor", "unregistered-provider"], "high").is_none());
        assert!(run::pool_effort_refusal(&[], "high").is_none());
    }
    #[test]
    fn control_commands_require_piped_text_and_a_channel() {
        let attempt = "11111111111111111111111111111111";
        valid(&["steer", attempt, "--text-stdin", "--operation-id", "s-1"]);
        invalid(&["steer", attempt]);
        valid(&[
            "chair",
            "message",
            "chair-a",
            "--text-stdin",
            "--channel",
            "console",
        ]);
        invalid(&["chair", "message", "chair-a", "--text-stdin"]);
        invalid(&["chair", "message", "chair-a", "--channel", "console"]);
        invalid(&["steer", attempt, "--text-stdin", "--approve"]);
    }
    #[test]
    fn mail_cursors_can_be_scoped_to_one_chair_session() {
        valid(&["fabric", "mail", "read", "--session", "chair-a"]);
        valid(&[
            "fabric",
            "mail",
            "mark-read",
            "3",
            "--session",
            "chair-a",
            "--operation-id",
            "mark-1",
        ]);
        invalid(&["fabric", "mail", "read", "--session"]);
    }
    #[test]
    fn full_help_publishes_provider_controls_and_scoped_help_does_not() {
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "--all", "--json"]) else {
            panic!("catalog")
        };
        assert_eq!(
            catalog.provider_controls,
            provenant_records::control::control_capabilities()
        );
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "steer", "--json"]) else {
            panic!("catalog")
        };
        assert!(catalog.provider_controls.is_empty());
        assert!(
            catalog
                .commands
                .iter()
                .any(|c| c.command == "provenant steer")
        );
    }
    #[test]
    fn native_claude_import_requires_unfiltered_stats() {
        valid(&["telemetry", "stats", "--claude-subagents", "subagents"]);
        for filter in [
            vec!["--usage"],
            vec!["--since", "2026-10-24"],
            vec!["--until", "2026-10-24"],
            vec!["--run-id", "11111111111111111111111111111111"],
        ] {
            let mut args = vec!["telemetry", "stats", "--claude-subagents", "subagents"];
            args.extend(filter);
            invalid(&args);
        }
    }
    #[test]
    fn telemetry_stats_parses_every_grouping_dimension() {
        for dimension in [
            "route", "pr", "issue", "graph", "node", "pattern", "preset", "role",
        ] {
            let invocation = valid(&["telemetry", "stats", "--by", dimension]);
            let Command::Telemetry(Telemetry::Stats(stats)) = invocation.command else {
                panic!("telemetry stats");
            };
            assert!(stats.by.is_some(), "{dimension}");
        }
    }
    #[test]
    fn experiment_show_parses_the_experiment_id_and_comparison_options() {
        let invocation = valid(&[
            "telemetry",
            "experiments",
            "show",
            "retry-shape",
            "--min-samples",
            "5",
        ]);
        let Command::Telemetry(Telemetry::Experiments(Experiments::Show(show))) =
            invocation.command
        else {
            panic!("telemetry experiments show");
        };
        assert_eq!(show.id, "retry-shape");
        assert_eq!(show.pipeline.min_samples, 5);
    }
    #[test]
    fn paging_recovery_and_removal_require_explicit_scopes() {
        invalid(&["status", "--until", "all"]);
        invalid(&["status", "--wait=1", "--cursor", "1"]);
        invalid(&["worktree", "remove", "--merged", "--integration", "main"]);
        valid(&[
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
            "--branch",
            "lane",
        ]);
        valid(&["worktree", "remove", "lane", "--integration", "main"]);
        invalid(&["worktree", "remove", "--branch", "lane"]);
        invalid(&["check", "release", "--base", &"a".repeat(40)]);
        invalid(&[
            "telemetry",
            "stats",
            "--since",
            "2026-10-24",
            "--until",
            "2026-10-24",
        ]); // timebomb: allow tests the equal-window boundary
        invalid(&["telemetry", "stats", "--since", "2026-10-24T00:00:00"]); // timebomb: allow offset requirement
    }
    #[test]
    fn collection_caps_are_not_per_occurrence_caps() {
        for (prefix, flag, max) in [
            (vec!["reconcile"], "--id", 100),
            (
                vec!["worktree", "remove", "--merged", "--integration", "main"],
                "--branch",
                100,
            ),
        ] {
            let id = "a".repeat(32);
            let mut words = prefix;
            for _ in 0..max {
                words.extend([flag, &id]);
            }
            valid(&words);
            words.extend([flag, &id]);
            invalid(&words);
        }
        let target = "a".repeat(32);
        let reason = "é".repeat(129);
        invalid(&["stop", &target, "--operation-id", "op", "--reason", &reason]);
    }
    #[test]
    fn parsed_globals_survive_help_version_and_failures() {
        for tail in [
            vec!["status", "--help"],
            vec!["--version"],
            vec!["help", "status"],
            vec!["status", "--limit", "0"],
        ] {
            let mut words = vec![
                "--state-dir",
                "/fixture/state",
                "--project",
                "/fixture/project",
                "--json",
            ];
            words.extend(tail);
            let globals = match parsed(&words) {
                ParseOutcome::Help { globals, .. }
                | ParseOutcome::Version { globals }
                | ParseOutcome::Invalid { globals, .. } => globals,
                other => panic!("unexpected parse result: {other:?}"),
            };
            assert!(globals.json);
            assert_eq!(
                globals.state_dir.as_deref(),
                Some(std::path::Path::new("/fixture/state"))
            );
            assert_eq!(
                globals.project.as_deref(),
                Some(std::path::Path::new("/fixture/project"))
            );
        }
        let ParseOutcome::Invalid { globals, .. } = parsed(&[
            "--state-dir",
            "/fixture/state",
            "--json",
            "status",
            "--json",
        ]) else {
            panic!("invalid")
        };
        assert_eq!(
            globals.state_dir.as_deref(),
            Some(std::path::Path::new("/fixture/state"))
        );
        let ParseOutcome::Invalid { error, .. } = parsed(&["--json", "status", "--unknown"]) else {
            panic!("invalid")
        };
        assert!(!error.to_string().contains("Usage:"));
    }
    #[test]
    fn usage_verbs_include_optional_parents_only() {
        let verbs = usage_verbs();
        assert!(verbs.contains(&"hosts".to_owned()));
        assert!(verbs.contains(&"hosts doctor".to_owned()));
        assert!(!verbs.contains(&"accounts".to_owned()));
        assert!(!verbs.contains(&"fabric".to_owned()));
    }
    #[test]
    fn help_all_lists_each_error_code_once_with_its_meaning() {
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "--all", "--json"]) else {
            panic!("catalog")
        };
        let codes: Vec<_> = catalog
            .error_codes
            .iter()
            .map(|c| c.code.as_str())
            .collect();
        assert_eq!(codes.len(), provenant_run::ErrorKind::ALL.len());
        let unique: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len(), "duplicate code in {codes:?}");
        for code in ["REFUSED", "DISK_FLOOR", "LANE_CAP", "HOST_CLOCK"] {
            assert!(codes.contains(&code), "{code} missing from {codes:?}");
        }
        assert!(catalog.error_codes.iter().all(|c| !c.summary.is_empty()));
    }
    #[test]
    fn globals_help_and_json_transport_are_pure_and_exact() {
        for words in [
            &["status", "--json", "--limit", "0"][..],
            &["--json", "status", "--limit", "0"],
        ] {
            assert!(matches!(
                parsed(words),
                ParseOutcome::Invalid {
                    globals: Globals { json: true, .. },
                    ..
                }
            ));
        }
        assert!(matches!(
            parsed(&["help", "--", "--json"]),
            ParseOutcome::Invalid {
                globals: Globals { json: false, .. },
                ..
            }
        ));
        assert!(matches!(
            parsed(&["hook", "claude", "pre-tool-use", "--help", "--json"]),
            ParseOutcome::Help {
                globals: Globals { json: true, .. },
                ..
            }
        ));
        invalid(&["hook", "claude", "pre-tool-use", "--json"]);
        invalid(&["internal", "supervise", "--json"]);
        invalid(&["-h"]);
        invalid(&["status", "--lim", "1"]);
        invalid(&["status", "--limit", "1", "--limit", "2"]);
        invalid(&["--json", "status", "--json"]);
        invalid(&["--state-dir", "/a", "status", "--state-dir", "/b"]);
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "--all", "--json"]) else {
            panic!("catalog")
        };
        assert!(
            !catalog
                .commands
                .iter()
                .any(|command| command.command.contains("internal"))
        );
        let status = catalog
            .commands
            .iter()
            .find(|command| command.command == "provenant status");
        assert!(
            status
                .is_some_and(|command| { command.arguments.iter().any(|arg| arg.name == "wait") })
        );
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "reconcile", "--json"]) else {
            panic!("reconcile help")
        };
        assert_eq!(catalog.commands.len(), 1);
        assert!(catalog.error_codes.is_empty(), "scoped help omits codes");
        let names = |words: &[&str]| match parsed(words) {
            ParseOutcome::Help { catalog, .. } => catalog
                .commands
                .into_iter()
                .map(|c| c.command)
                .collect::<Vec<_>>(),
            other => panic!("{words:?}: {other:?}"),
        };
        let mail = names(&["help", "fabric", "mail", "--json"]);
        assert!(mail.contains(&"provenant fabric mail send".to_owned()));
        assert!(mail.iter().all(|c| c.starts_with("provenant fabric mail")));
        assert_eq!(
            names(&["help", "hosts", "--json"]),
            vec![
                "provenant hosts".to_owned(),
                "provenant hosts doctor".to_owned()
            ]
        );
        assert_eq!(
            names(&["fabric", "mail", "send", "--help", "--json"]),
            vec!["provenant fabric mail send".to_owned()]
        );
        assert!(names(&["hosts", "--help", "--json"]).len() == 2);
        assert_eq!(
            names(&["--project", "hosts", "status", "--help", "--json"]),
            vec!["provenant status".to_owned()]
        );
        assert_eq!(
            names(&["hosts", "--help", "doctor", "--json"]).len(),
            2,
            "words after the first --help are ignored"
        );
        assert!(matches!(
            parsed(&["fabric", "peer", "--help", "--json"]),
            ParseOutcome::Invalid { .. }
        ));
        assert!(matches!(
            parsed(&["help", "nope", "--json"]),
            ParseOutcome::Invalid { .. }
        ));
    }
    #[cfg(unix)]
    #[test]
    fn byte_paths_survive_clap_without_text_conversion() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![b'/', b'f', 0xff]);
        let ParseOutcome::Invocation(inv) = parse([
            OsString::from("--state-dir"),
            raw.clone(),
            OsString::from("root"),
        ]) else {
            panic!("byte path")
        };
        assert_eq!(inv.globals.state_dir.map(|v| v.into_os_string()), Some(raw));
    }
}

pub mod hooks;
