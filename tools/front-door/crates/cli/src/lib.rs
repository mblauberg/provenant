//! Pure argument parsing, help and bounded result rendering. No owner execution.
#![forbid(unsafe_code)]
mod grammar;
mod render;
use clap::{Arg, ArgAction, ArgGroup, CommandFactory, FromArgMatches};
pub use grammar::*;
use provenant_records::{HelpArgument, HelpCatalog, HelpCommand, HelpGroup};
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
        catalog: HelpCatalog,
        json: bool,
    },
    Version {
        json: bool,
    },
    Invalid {
        error: CliError,
        json: bool,
    },
}
/// Iterator contains arguments after argv[0]. Only the JSON transport bit is
/// pre-scanned: values and command semantics belong to Clap and typed validation.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> ParseOutcome {
    let args: Vec<_> = args.into_iter().collect();
    let json = args
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json");
    let mut model = command_model();
    let mut argv = vec![OsString::from("provenant")];
    argv.extend(args);
    let matches = match model.try_get_matches_from_mut(argv) {
        Ok(matches) => matches,
        Err(error) => {
            return match error.kind() {
                clap::error::ErrorKind::DisplayHelp => ParseOutcome::Help {
                    text: error.to_string(),
                    catalog: catalog(&model),
                    json,
                },
                clap::error::ErrorKind::DisplayVersion => ParseOutcome::Version { json },
                _ => ParseOutcome::Invalid {
                    error: CliError::Invalid(error.to_string()),
                    json,
                },
            };
        }
    };
    let mut invocation = match RawInvocation::from_arg_matches(&matches) {
        Ok(value) => value,
        Err(error) => {
            return ParseOutcome::Invalid {
                error: CliError::Invalid(error.to_string()),
                json,
            };
        }
    };
    // Globals are local copies at every node so Clap does not overwrite a
    // parent's singleton with a child's. Collect the parsed values, not argv.
    fn globals(matches: &clap::ArgMatches, into: &mut RawGlobals) {
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
            globals(child, into);
        }
    }
    invocation.globals = RawGlobals {
        json: Vec::new(),
        state_dir: Vec::new(),
        project: Vec::new(),
    };
    globals(&matches, &mut invocation.globals);
    if invocation.globals.json.len() > 1
        || invocation.globals.state_dir.len() > 1
        || invocation.globals.project.len() > 1
    {
        return ParseOutcome::Invalid {
            error: CliError::Invalid("global options may occur only once".into()),
            json,
        };
    }
    let invocation = Invocation {
        globals: Globals {
            json: !invocation.globals.json.is_empty(),
            state_dir: invocation.globals.state_dir.into_iter().next(),
            project: invocation.globals.project.into_iter().next(),
        },
        command: invocation.command,
    };
    if let Command::Help { command } = &invocation.command {
        let mut selected = &mut model;
        for component in command {
            selected = match selected.find_subcommand_mut(component) {
                Some(child) if !child.is_hide_set() => child,
                _ => {
                    return ParseOutcome::Invalid {
                        error: CliError::Invalid("unknown help command".into()),
                        json,
                    };
                }
            };
        }
        return ParseOutcome::Help {
            text: selected.render_long_help().to_string(),
            catalog: catalog(&command_model()),
            json,
        };
    }
    match validate(&invocation) {
        Ok(()) => ParseOutcome::Invocation(invocation),
        Err(error) => ParseOutcome::Invalid { error, json },
    }
}
pub fn command_model() -> clap::Command {
    fn configure(mut command: clap::Command, globals: &[Arg]) -> clap::Command {
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
                        "network",
                        "capability",
                        "provider_timeout",
                    ])
                });
        }
        if command.get_name() == "remove" {
            command = command.group(
                ArgGroup::new("remove_scope")
                    .args(["branch", "since"])
                    .multiple(false),
            );
        }
        let children: Vec<_> = command
            .get_subcommands()
            .cloned()
            .map(|child| configure(child, globals))
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
    let mut command = configure(base, &globals)
        .arg(
            Arg::new("help")
                .long("help")
                .global(true)
                .action(ArgAction::HelpLong),
        )
        .arg(
            Arg::new("version")
                .long("version")
                .global(true)
                .action(ArgAction::Version),
        )
        .version(env!("CARGO_PKG_VERSION"));
    command.build();
    command
}
fn invalid(message: &str) -> Result<(), CliError> {
    Err(CliError::Invalid(message.into()))
}
fn route_valid(route: &RouteRequest) -> Result<(), CliError> {
    if route.model.is_none() && route.route.is_none() {
        return invalid("one of --model or --route is required");
    }
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
            Command::Hook(_) | Command::Internal(_) | Command::Fabric(Fabric::Mcp)
        )
    {
        return invalid("--json is invalid for hook/MCP/internal execution");
    }
    match &inv.command {
        Command::Route(Route::Resolve(route)) => route_valid(route)?,
        Command::Dispatch(dispatch) => {
            let access = &dispatch.access;
            if dispatch.input.manifest.is_some() {
                if dispatch.task_id.is_some()
                    || dispatch.delivery_id.is_some()
                    || dispatch.provider_timeout.is_some()
                    || dispatch.route.model.is_some()
                    || dispatch.route.route.is_some()
                    || dispatch.route.effort.is_some()
                    || !dispatch.route.fallback.is_empty()
                    || access.mode.is_some()
                    || access.network.is_some()
                    || access.worktree.is_some()
                    || !access.read_root.is_empty()
                    || !access.capability.is_empty()
                {
                    return invalid(
                        "manifest conflicts with per-task route, access, prompt metadata and provider timeout",
                    );
                }
            } else {
                if dispatch.concurrency.is_some() {
                    return invalid("--concurrency requires --manifest");
                }
                route_valid(&dispatch.route)?;
            }
            if (access.mode() == Mode::WorktreeWrite) != access.worktree.is_some() {
                return invalid(
                    "--worktree is required with, and only valid for, --mode worktree-write",
                );
            }
            if access.mode() == Mode::WorktreeWrite && !dispatch.route.fallback.is_empty() {
                return invalid("writers cannot use fallback models");
            }
            if access.read_root.len() > 32 || access.capability.len() > 16 {
                return invalid("at most 32 read roots and 16 capabilities are allowed");
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
fn catalog(model: &clap::Command) -> HelpCatalog {
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
                    bounds: bounds(&path, &name),
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
                    defaults: {
                        let mut values: Vec<_> = arg
                            .get_default_values()
                            .iter()
                            .map(|value| value.to_string_lossy().into_owned())
                            .collect();
                        if values.is_empty()
                            && let Some(value) = match arg.get_id().as_str() {
                                "mode" => Some("read-only"),
                                "network" => Some("off"),
                                "concurrency" => Some("4"),
                                "condition" => Some("any"),
                                "offset" => Some("0"),
                                _ => None,
                            }
                        {
                            values.push(value.into());
                        }
                        values
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
    HelpCatalog { commands }
}
fn constraints(path: &str) -> Vec<String> {
    let values: &[&str] = match path {
        "provenant dispatch" => &[
            "single prompt requires exactly one of model or route",
            "manifest conflicts with per-task route, access, metadata and provider-timeout",
            "concurrency requires manifest",
            "worktree is required with, and only valid for, worktree-write",
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
        "read_root" => Some((0, 32)),
        "capability" => Some((0, 16)),
        "fallback" => Some((0, 2)),
        "id" | "branch"
            if path.ends_with("lanes")
                || path.ends_with("reconcile")
                || path.ends_with("remove") =>
        {
            Some((0, 100))
        }
        "paths" => Some((1, 256)),
        "reason" => Some((0, 256)),
        _ => None,
    }
}
fn requirements(path: &str, name: &str) -> Vec<String> {
    let names: &[&str] = match (path, name) {
        (_, "condition") => &["wait"],
        (_, "concurrency") => &["manifest"],
        (_, "abandon_uncertain") => &["id"],
        ("provenant check release", "base") => &["head"],
        ("provenant check release", "head") => &["base"],
        ("provenant worktree remove", "merged") => &["integration", "remove_scope"],
        ("provenant worktree remove", "integration" | "branch" | "since") => &["merged"],
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
        assert_eq!(request.provider_timeout(), 3600);
        assert_eq!(request.concurrency(), 4);
        let mut words = base.to_vec();
        words.extend(["--mode", "worktree-write"]);
        invalid(&words);
        words.extend(["--worktree", "/fixture/worktree"]);
        let inv = valid(&words);
        let Command::Dispatch(request) = inv.command else {
            panic!("dispatch")
        };
        assert_eq!(request.provider_timeout(), 10800);
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
        invalid(&["reconcile", "--abandon-uncertain"]);
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
        invalid(&["worktree", "remove", "lane", "--integration", "main"]);
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
    fn globals_help_and_json_transport_are_pure_and_exact() {
        for words in [
            &["lanes", "--json", "--limit", "0"][..],
            &["--json", "lanes", "--limit", "0"],
        ] {
            assert!(matches!(
                parsed(words),
                ParseOutcome::Invalid { json: true, .. }
            ));
        }
        assert!(matches!(
            parsed(&["help", "--", "--json"]),
            ParseOutcome::Invalid { json: false, .. }
        ));
        assert!(matches!(
            parsed(&["hook", "claude", "pre-tool-use", "--help", "--json"]),
            ParseOutcome::Help { json: true, .. }
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
