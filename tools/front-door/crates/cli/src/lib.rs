//! Pure argument parsing, help and bounded result rendering. No owner execution.
#![forbid(unsafe_code)]
pub mod accounts;
pub mod bench;
pub mod check;
pub mod domains;
pub mod fabric;
pub mod quota;
pub mod route;
pub mod run;
pub mod workflow;
pub use bench::Bench;
pub use fabric::Fabric;
pub use quota::Quota;
pub use route::Route;
pub use workflow::Workflow;
mod grammar;
pub mod install;
mod render;
use clap::{Arg, ArgAction, ArgGroup, CommandFactory, FromArgMatches};
pub use grammar::*;
use provenant_records::{HelpArgument, HelpCatalogue, HelpCommand, HelpGroup};
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
    let args: Vec<_> = args.into_iter().collect();
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
            let recovered = build_model(true)
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
                        text: error.to_string(),
                        catalog: scoped(catalog(&model), &scope),
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
    if let Command::Help { command } = &invocation.command {
        let mut selected = &mut model;
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
            text: selected.render_long_help().to_string(),
            catalog: scoped(catalog(&command_model()), &scope),
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
    CliError::Invalid(message.strip_prefix("error: ").unwrap_or(message).into())
}
fn parsed_globals(matches: &clap::ArgMatches, json: bool) -> (Globals, Option<CliError>) {
    fn collect(matches: &clap::ArgMatches, into: &mut RawGlobals) {
        if let Some(values) = matches.get_many::<bool>("json") {
            into.json.extend(values.copied());
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
        state_dir: Vec::new(),
        project: Vec::new(),
    };
    collect(matches, &mut raw);
    let duplicate = (raw.json.len() > 1 || raw.state_dir.len() > 1 || raw.project.len() > 1)
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
            state_dir: unique(raw.state_dir),
            project: unique(raw.project),
        },
        duplicate,
    )
}

pub fn command_model() -> clap::Command {
    build_model(false)
}
fn build_model(passive: bool) -> clap::Command {
    fn configure(mut command: clap::Command, globals: &[Arg], parent: &str) -> clap::Command {
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
                    .args(["model", "route"])
                    .required(true),
            );
        }
        if command.get_name() == "dispatch" {
            command = command
                .mut_arg("model", |arg| {
                    arg.required_unless_present_any(["route", "manifest"])
                })
                .mut_arg("manifest", |arg| {
                    arg.conflicts_with_all([
                        "task_id",
                        "delivery_id",
                        "model",
                        "route",
                        "effort",
                        "fallback",
                        "mode",
                        "worktree",
                        "read_root",
                        "write_root",
                        "network",
                        "capability",
                        "provider_timeout",
                    ])
                });
        }
        if path == "provenant worktree remove" {
            command = command.group(
                ArgGroup::new("remove_scope")
                    .args(["branch", "since"])
                    .multiple(false),
            );
        }
        let children: Vec<_> = command
            .get_subcommands()
            .cloned()
            .map(|child| configure(child, globals, &path))
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
        .filter(|arg| matches!(arg.get_id().as_str(), "json" | "state_dir" | "project"))
        .cloned()
        .map(|arg| arg.global(false))
        .collect();
    let mut command = configure(base, &globals, "")
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
        Command::Route(Route::Resolve(route)) => route_valid(route)?,
        Command::Dispatch(dispatch) => {
            let access = &dispatch.access;
            route_valid(&dispatch.route)?;
            if (access.mode == Mode::WorktreeWrite)
                != (access.worktree.is_some() || dispatch.branch.is_some())
            {
                return invalid(
                    "--worktree, or a peer writer's --branch, is required with, and only valid \
                     for, --mode worktree-write",
                );
            }
            if access.mode == Mode::WorktreeWrite && !dispatch.route.fallback.is_empty() {
                return invalid("writers cannot use fallback models");
            }
            if access.read_root.len() > 32
                || access.write_root.len() > 32
                || access.capability.len() > 16
            {
                return invalid(
                    "at most 32 read roots, 32 write roots and 16 capabilities are allowed",
                );
            }
        }
        Command::Lanes(lanes) if lanes.id.len() > 100 => {
            return invalid("at most 100 IDs are allowed");
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
            if child.is_hide_set() {
                return None;
            }
            scope = format!("{scope} {}", child.get_name());
            current = child;
        }
    }
    Some(scope)
}
/// Keep the named command and its descendants; `help` alone keeps all.
fn scoped(mut catalogue: HelpCatalogue, scope: &str) -> HelpCatalogue {
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
                    bounds: arg
                        .get_num_args()
                        .filter(|range| range.max_values() > 1 && range.max_values() < usize::MAX)
                        .map(|range| (range.min_values() as u64, range.max_values() as u64))
                        .or_else(|| bounds(&path, &name)),
                    conditional_defaults: if name == "provider_timeout" {
                        vec![
                            ("read-only".into(), "3600".into()),
                            ("worktree-write".into(), "10800".into()),
                        ]
                    } else {
                        Vec::new()
                    },
                    requires: requirements(&path, &name),
                    name,
                    long: arg.get_long().map(str::to_owned),
                    positional: arg.is_positional(),
                    required: arg.is_required_set(),
                    repeatable: !matches!(arg.get_id().as_str(), "json" | "state_dir" | "project")
                        && (matches!(arg.get_action(), ArgAction::Append)
                            || arg
                                .get_num_args()
                                .is_some_and(|range| range.max_values() > 1)),
                    defaults: arg
                        .get_default_values()
                        .iter()
                        .map(|value| value.to_string_lossy().into_owned())
                        .collect(),
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
    HelpCatalogue { commands }
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
    let values: &[&str] = match path {
        "provenant dispatch" => &[
            "single prompt requires exactly one of model or route",
            "manifest conflicts with per-task route, access, metadata and provider-timeout",
            "concurrency requires manifest",
            "worktree, or a peer writer's branch, is required with, and only valid for, worktree-write",
            "fallback conflicts with a writer and has at most two distinct models",
            "prompt and manifest inputs are at most 1 MiB",
        ],
        "provenant route resolve" => &[
            "exactly one of model or route",
            "fallback has at most two distinct models",
        ],
        "provenant telemetry stats" => &[
            "dates mean midnight UTC; timestamps need an offset",
            "since must precede until",
        ],
        "provenant resume" => &[
            "target is a full RUN/TASK/ATTEMPT",
            "prompt is at most 1 MiB; expected generation is 0",
        ],
        "provenant output" => &["target is a full RUN/TASK/ATTEMPT"],
        "provenant hook" | "provenant internal" => &["json transport is invalid for execution"],
        _ => &[],
    };
    values.iter().map(|value| (*value).into()).collect()
}
fn bounds(path: &str, name: &str) -> Option<(u64, u64)> {
    match name {
        "wait" | "provider_timeout" => Some((1, 14400)),
        "limit" => Some((1, 100)),
        "concurrency" => Some((1, 8)),
        "tail" => Some((1, 500)),
        "max_bytes" => Some((1, 65536)),
        "read_root" | "write_root" => Some((0, 32)),
        "capability" => Some((0, 16)),
        "fallback" => Some((0, 2)),
        "id" | "branch"
            if path.ends_with("lanes")
                || path.ends_with("reconcile")
                || path.ends_with("remove") =>
        {
            Some((0, 100))
        }
        "reason" => Some((0, 256)),
        _ => None,
    }
}
fn requirements(path: &str, name: &str) -> Vec<String> {
    let names: &[&str] = match (path, name) {
        (_, "condition") => &["wait"],
        (_, "concurrency") => &["manifest"],
        ("provenant check release", "base") => &["head"],
        ("provenant check release", "head") => &["base"],
        ("provenant worktree remove", "merged") => &["integration", "remove_scope"],
        ("provenant worktree remove", "branch" | "since") => &["merged"],
        _ => &[],
    };
    names.iter().map(|v| (*v).into()).collect()
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
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
        for wait in ["1", "14400"] {
            valid(&["lanes", "--wait", wait]);
        }
        for wait in ["0", "14401", "-1", "1.5", "18446744073709551616"] {
            invalid(&["lanes", "--wait", wait]);
        }
        valid(&["lanes", "--limit", "100"]);
        invalid(&["lanes", "--limit", "101"]);
        let attempt = "11".repeat(16) + &format!("/{}/{}", "22".repeat(16), "33".repeat(16));
        valid(&["output", &attempt, "--tail", "500", "--max-bytes", "65536"]);
        invalid(&["output", &attempt, "--tail", "501"]);
        invalid(&["output", &attempt, "--max-bytes", "65537"]);
        invalid(&["output", &attempt, "--offset", "0", "--tail", "1"]);
        invalid(&["output", &"11".repeat(16)]);
    }
    #[test]
    fn lanes_accept_repeatable_state_and_since_filters() {
        valid(&[
            "lanes",
            "--all",
            "--state",
            "running",
            "--state",
            "input_required",
            "--since",
            "24h",
        ]);
        valid(&["lanes", "--since", "2026-10-25T00:00:00.000000Z"]);
        invalid(&["lanes", "--since"]);
        invalid(&["lanes", "--state", "unknown"]);
        invalid(&["lanes", "--wait", "10", "--since", "24h"]);
    }
    #[test]
    fn lanes_host_lists_a_peer_without_ids_waits_or_cursors() {
        valid(&["lanes", "--host", "workshop"]);
        valid(&["lanes", "--host", "workshop", "--all", "--state", "running"]);
        let run = "11".repeat(16);
        invalid(&["lanes", "--host", "workshop", "--id", &run]);
        invalid(&["lanes", "--host", "workshop", "--wait", "5"]);
        invalid(&["lanes", "--host", "workshop", "--cursor", "1"]);
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
    fn manifest_and_writer_authority_do_not_mix() {
        valid(&[
            "route",
            "resolve",
            "--model",
            "opencode/openrouter/anthropic/model",
        ]);
        valid(&[
            "dispatch",
            "--manifest",
            "tasks.json",
            "--operation-id",
            "op",
            "--concurrency",
            "8",
        ]);
        for args in [
            vec!["--model", "codex/id"],
            vec!["--mode", "read-only"],
            vec!["--network", "off"],
            vec!["--provider-timeout", "1"],
            vec!["--effort", "high"],
        ] {
            let mut words = vec![
                "dispatch",
                "--manifest",
                "tasks.json",
                "--operation-id",
                "op",
            ];
            words.extend(args);
            invalid(&words);
        }
        let base = [
            "dispatch",
            "--prompt-stdin",
            "--operation-id",
            "op",
            "--model",
            "codex/id",
        ];
        let inv = valid(&base);
        let Command::Dispatch(request) = inv.command else {
            panic!("dispatch")
        };
        assert_eq!(request.provider_timeout, 3600);
        assert_eq!(request.concurrency, None);
        let mut words = base.to_vec();
        words.extend(["--mode", "worktree-write"]);
        invalid(&words);
        words.extend(["--worktree", "/fixture/worktree"]);
        let inv = valid(&words);
        let Command::Dispatch(request) = inv.command else {
            panic!("dispatch")
        };
        assert_eq!(request.provider_timeout, 10800);
        words.extend(["--fallback", "claude/id"]);
        invalid(&words);
        let mut words = base.to_vec();
        words.extend(["--worktree", "/fixture"]);
        invalid(&words);
        let mut words = base.to_vec();
        words.extend(["--concurrency", "4"]);
        invalid(&words);
        let mut words = base.to_vec();
        words.extend(["--fallback", "a/id,a/id"]);
        invalid(&words);
        let mut words = base.to_vec();
        words.extend(["--fallback", "a/id,b/id,c/id"]);
        invalid(&words);
        let mut words = base.to_vec();
        words.extend(["--fallback", "a/id", "--fallback", "b/id"]);
        invalid(&words);
    }
    #[test]
    fn paging_recovery_and_removal_require_explicit_scopes() {
        invalid(&["lanes", "--condition", "all"]);
        invalid(&["lanes", "--wait", "1", "--cursor", "next"]);
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
            (vec!["lanes"], "--id", 100),
            (vec!["reconcile"], "--id", 100),
            (
                vec!["worktree", "remove", "--merged", "--integration", "main"],
                "--branch",
                100,
            ),
            (
                vec![
                    "dispatch",
                    "--prompt-stdin",
                    "--operation-id",
                    "op",
                    "--model",
                    "a/id",
                ],
                "--read-root",
                32,
            ),
            (
                vec![
                    "dispatch",
                    "--prompt-stdin",
                    "--operation-id",
                    "op",
                    "--model",
                    "a/id",
                ],
                "--capability",
                16,
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
        invalid(&[
            "cancel",
            &target,
            "--operation-id",
            "op",
            "--reason",
            &reason,
        ]);
    }
    #[test]
    fn parsed_globals_survive_help_version_and_failures() {
        for tail in [
            vec!["lanes", "--help"],
            vec!["--version"],
            vec!["help", "lanes"],
            vec!["lanes", "--limit", "0"],
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
        let ParseOutcome::Invalid { globals, .. } =
            parsed(&["--state-dir", "/fixture/state", "--json", "lanes", "--json"])
        else {
            panic!("invalid")
        };
        assert_eq!(
            globals.state_dir.as_deref(),
            Some(std::path::Path::new("/fixture/state"))
        );
        let ParseOutcome::Invalid { error, .. } = parsed(&["--json", "lanes", "--unknown"]) else {
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
    fn globals_help_and_json_transport_are_pure_and_exact() {
        for words in [
            &["lanes", "--json", "--limit", "0"][..],
            &["--json", "lanes", "--limit", "0"],
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
        invalid(&["lanes", "--lim", "1"]);
        invalid(&["lanes", "--limit", "1", "--limit", "2"]);
        invalid(&["--json", "lanes", "--json"]);
        invalid(&["--state-dir", "/a", "lanes", "--state-dir", "/b"]);
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "--json"]) else {
            panic!("catalog")
        };
        assert!(
            !catalog
                .commands
                .iter()
                .any(|command| command.command.contains("internal"))
        );
        let lanes = catalog
            .commands
            .iter()
            .find(|command| command.command == "provenant lanes");
        assert!(lanes.is_some_and(|command| {
            command
                .arguments
                .iter()
                .any(|arg| arg.name == "wait" && arg.bounds == Some((1, 14400)))
        }));
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "reconcile", "--json"]) else {
            panic!("reconcile help")
        };
        assert_eq!(catalog.commands.len(), 1);
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
            names(&["--project", "hosts", "lanes", "--help", "--json"]),
            vec!["provenant lanes".to_owned()]
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
