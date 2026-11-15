//! Pure argument parsing, help and bounded result rendering. No owner execution.
#![forbid(unsafe_code)]
use provenant_core::edit_distance;
pub mod accounts;
pub mod brief;
pub mod check;
pub mod cloud;
pub mod config_reads;
pub mod console;
pub mod domains;
pub mod external_contract;
pub mod fabric;
pub mod ignore_files;
pub mod quota;
pub(crate) mod quota_history;
pub mod route;
pub mod run;
pub mod target;
pub mod tracker;
pub mod wait;
pub mod workflow;
pub use cloud::Cloud;
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
pub mod telemetry_args;
pub mod telemetry_value;
#[cfg(test)]
mod test_support;
use clap::{Arg, ArgAction, ArgGroup, CommandFactory, FromArgMatches};
pub use grammar::*;
pub use provenant_config::removed::{REMOVED, Removed};
use provenant_records::{HelpArgument, HelpCatalogue, HelpCommand, HelpErrorCode, HelpGroup};
pub use render::*;
use std::ffi::OsString;
pub use telemetry_args::CostBy;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{message}")]
    Invalid {
        message: String,
        repair: Option<String>,
    },
    #[error("{owner} owner is unavailable in this build")]
    OwnerUnavailable { owner: &'static str },
}

impl CliError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
            repair: None,
        }
    }

    pub fn invalid_with_repair(message: impl Into<String>, repair: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
            repair: Some(repair.into()),
        }
    }

    pub fn repair(&self) -> Option<&str> {
        match self {
            Self::Invalid { repair, .. } => repair.as_deref(),
            Self::OwnerUnavailable { .. } => None,
        }
    }
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
                    Some(scope) => {
                        let (text, catalog) = if scope == "provenant" {
                            (core_help(&model), catalog(&model))
                        } else if is_core_scope(&model, &scope) {
                            (
                                error.to_string(),
                                scoped_catalogue(compact_help_model(), &scope),
                            )
                        } else {
                            expanded_scoped_help(&scope)
                        };
                        let text = if is_core_scope(&model, &scope) {
                            append_child_index(text, &scope)
                        } else {
                            text
                        };
                        ParseOutcome::Help {
                            text,
                            catalog,
                            globals,
                        }
                    }
                    None => ParseOutcome::Invalid {
                        error: CliError::invalid("unknown help command"),
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
        let scope = format!("provenant {}", command.join(" "));
        let mut expanded = if *all || (!command.is_empty() && !is_core_scope(&model, &scope)) {
            expanded_model()
        } else {
            compact_help_model()
        };
        let mut selected = &mut expanded;
        let mut scope = String::from("provenant");
        for component in command {
            selected = match selected.find_subcommand_mut(component) {
                Some(child) if !child.is_hide_set() || scope == "provenant quota" => {
                    *child = child.clone().hide(false);
                    scope = format!("{scope} {}", child.get_name());
                    child
                }
                _ => {
                    return ParseOutcome::Invalid {
                        error: CliError::invalid("unknown help command"),
                        globals: invocation.globals,
                    };
                }
            };
        }
        let text = if command.is_empty() && !all {
            core_help(&model)
        } else if command.is_empty() && *all {
            all_command_help(selected)
        } else {
            selected.render_long_help().to_string()
        };
        let text = if !command.is_empty() && !all && is_core_scope(&model, &scope) {
            append_child_index(text, &scope)
        } else {
            text
        };
        return ParseOutcome::Help {
            text,
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

/// The removed spelling `argv` uses, read from its command words and options.
fn removed_spelling(argv: &[OsString]) -> Option<&'static Removed> {
    let model = expanded_model();
    let mut words = Vec::new();
    let mut flags = Vec::new();
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        let word = arg.to_str()?;
        if word == "--" {
            break;
        }
        if let Some(option) = word.strip_prefix("--") {
            let (name, joined) = option
                .split_once('=')
                .map_or((option, false), |(name, _)| (name, true));
            // Skip the value of a global option, which is never a command word.
            if !joined
                && model.get_arguments().any(|arg| {
                    arg.get_long() == Some(name)
                        && arg.get_num_args().map_or_else(
                            || arg.get_action().takes_values(),
                            |range| range.min_values() > 0,
                        )
                })
            {
                args.next();
            }
            flags.push(name);
        } else if !word.starts_with('-') {
            words.push(word);
        }
    }
    REMOVED
        .iter()
        .filter(|removed| !removed.words.is_empty() || removed.flag.is_some())
        .find(|removed| {
            words.len() >= removed.words.len()
                && words
                    .iter()
                    .zip(removed.words)
                    .all(|(word, path)| word == path)
                && removed.flag.is_none_or(|flag| flags.contains(&flag))
        })
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

    if let Some(removed) = removed_spelling(argv) {
        return CliError::Invalid {
            // The message names the replacement, so the error stays one line.
            message: removed.message(),
            repair: None,
        };
    }

    if matches!(
        error.kind(),
        clap::error::ErrorKind::MissingSubcommand
            | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        let model = expanded_model();
        let (selected, path) =
            walk_subcommands(&model, argv, None).unwrap_or((&model, "provenant".into()));
        let first_sub = selected
            .get_subcommands()
            .find(|child| !child.is_hide_set())
            .map(|child| child.get_name());
        let correction = if path == "provenant config" {
            "provenant config show".to_owned()
        } else {
            match first_sub {
                Some(sub) => format!("{path} {sub}"),
                None => path.clone(),
            }
        };
        let usage_block = selected.clone().bin_name(&path).render_help().to_string();
        let message =
            format!("'{path}' requires a subcommand\n\n{usage_block}\nexample: {correction}");
        return CliError::Invalid {
            message,
            repair: Some(correction),
        };
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
    let mut message = match &example {
        Some(example) => format!("{message}\nexample: {example}"),
        None => message.into(),
    };
    if let Some(suggestion) = suggestion {
        message.push_str(&format!("\ndid you mean `{suggestion}`?"));
    }
    CliError::Invalid {
        message,
        repair: example,
    }
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
                        && arg.get_num_args().map_or_else(
                            || arg.get_action().takes_values(),
                            |range| range.min_values() > 0,
                        )
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
    let (selected, mut path) = walk_subcommands(&model, argv, None)?;
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
    if lands {
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
            let proposed = format!("--wait={value}");
            let command = format!("provenant land 2199 {proposed}");
            let words = command
                .split_whitespace()
                .map(OsString::from)
                .collect::<Vec<_>>();
            return Some(if command_model().try_get_matches_from(words).is_ok() {
                command
            } else {
                "provenant land 2199 --wait=30m".into()
            });
        }
    }

    // Preserve each argument only when the resulting invocation parses.
    if argv
        .iter()
        .any(|arg| arg.to_str().is_some_and(|s| s.starts_with("--wait=")))
    {
        let fixed: Vec<OsString> = argv
            .iter()
            .map(|arg| {
                if arg.to_str().is_some_and(|s| s.starts_with("--wait=")) {
                    OsString::from("--wait=30m")
                } else {
                    arg.clone()
                }
            })
            .collect();
        if command_model().try_get_matches_from(fixed.clone()).is_ok()
            && matches!(
                parse(fixed.iter().skip(1).cloned()),
                ParseOutcome::Invocation(_)
            )
        {
            let quoted: Option<Vec<String>> = fixed
                .iter()
                .map(|arg| {
                    arg.to_str().map(|s| {
                        if !s.is_empty()
                            && s.bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_./=-".contains(&b))
                        {
                            s.to_owned()
                        } else {
                            format!("'{}'", s.replace('\'', "'\\''"))
                        }
                    })
                })
                .collect();
            if let Some(quoted) = quoted {
                return Some(quoted.join(" "));
            }
        }
    }

    match path.as_str() {
        "provenant land" | "provenant pr land" => Some("provenant land 2199 --wait".into()),
        "provenant route resolve" => Some("provenant route resolve senior".into()),
        "provenant output" => Some("provenant output fix-login/review".into()),
        "provenant steer" => Some("provenant steer fix-login/implement --brief-file -".into()),
        "provenant run" => Some("provenant run task 'Meet the acceptance criteria'".into()),
        _ => {
            if let Some(core) = selected.get::<metadata::CoreCommand>() {
                return Some(core.1.to_string());
            }
            if let Some(first_child) = selected.get_subcommands().find(|c| !c.is_hide_set()) {
                return Some(format!("{path} {}", first_child.get_name()));
            }
            let mut parts = vec![path.clone()];
            for arg in selected.get_arguments() {
                if arg.is_required_set() && !arg.is_hide_set() {
                    let name = arg.get_id().as_str();
                    let token = match name {
                        "number" | "numbers" => "2199",
                        "target" | "targets" => "NAME",
                        "brief" | "prompt" => "'Task'",
                        "branch" => "BRANCH",
                        "host" => "HOST",
                        "file" | "path" => "FILE",
                        _ => name,
                    };
                    if let Some(long) = arg.get_long() {
                        parts.push(format!("--{long} {token}"));
                    } else {
                        parts.push(token.to_string());
                    }
                }
            }
            Some(parts.join(" "))
        }
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
        .then(|| CliError::invalid("global options may occur only once"));
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
    let arguments: Vec<_> = model
        .get_arguments()
        .filter(|arg| !arg.is_hide_set() && arg.get_long().is_some())
        .collect();
    if !arguments.is_empty() {
        text.push_str("\nOptions:\n");
        for arg in arguments {
            let Some(long) = arg.get_long() else {
                continue;
            };
            let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
            text.push_str(&format!("  --{long:<16} {help}\n"));
        }
    }
    text.push_str(
        "  help --all  All commands and advanced options\n              provenant help --all\n",
    );
    text
}
fn all_command_help(model: &clap::Command) -> String {
    fn append(command: &clap::Command, path: String, text: &mut String) {
        if command.is_hide_set() {
            return;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(
            &command
                .clone()
                .bin_name(path.clone())
                .render_long_help()
                .to_string(),
        );
        for child in command.get_subcommands() {
            append(child, format!("{path} {}", child.get_name()), text);
        }
    }
    let mut text = String::new();
    append(model, model.get_name().to_owned(), &mut text);
    text.push_str("\nFor focused command help, run provenant help COMMAND.\n");
    text
}
fn expanded_model() -> clap::Command {
    build_model(false, true)
}
fn compact_help_model() -> clap::Command {
    build_model(false, false)
}
fn build_model(passive: bool, advanced: bool) -> clap::Command {
    // Compact help keeps the command catalogue small, while each command's
    // arguments remain visible directly from the clap grammar.
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
        if path == "provenant wait" {
            command = command.after_help(WAIT_JSON_CONTRACT);
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
        if !advanced
            && (parent == "provenant quota"
                || (parent == "provenant" && command.get::<metadata::CoreCommand>().is_none()))
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
    Err(CliError::invalid(message))
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
    if inv.globals.json && matches!(inv.command, Command::Hook(_) | Command::Internal(_)) {
        return invalid("--json is invalid for hook/internal execution");
    }
    match &inv.command {
        Command::Route(Route::Resolve(resolve)) => {
            if resolve.name.is_none()
                && resolve.request.model.is_none()
                && resolve.request.route.is_none()
            {
                return Err(CliError::invalid_with_repair(
                    "route resolve needs a route or model, such as `route resolve utility`, \
                     `route resolve codex/gpt-6-luna`, `--route utility` or `--model ADAPTER/ID`",
                    "provenant route resolve --model ADAPTER/ID",
                ));
            }
            route_valid(&resolve.request)?
        }
        Command::Reconcile(reconcile) if reconcile.targets.len() > 100 => {
            return invalid("at most 100 targets are allowed");
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
/// global `--state-dir` and `--project`. Explicit quota subcommands remain
/// addressable even though they are omitted from the compact listing.
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
            if child.get_name() == "internal"
                || (child.is_hide_set()
                    && !matches!(scope.as_str(), "provenant" | "provenant quota"))
            {
                return None;
            }
            scope = format!("{scope} {}", child.get_name());
            current = child;
        }
    }
    Some(scope)
}
fn is_core_scope(model: &clap::Command, scope: &str) -> bool {
    let mut selected = model;
    for component in scope.split_whitespace().skip(1) {
        let Some(child) = selected.find_subcommand(component) else {
            return false;
        };
        selected = child;
    }
    selected.get::<metadata::CoreCommand>().is_some()
}
fn expanded_scoped_help(scope: &str) -> (String, HelpCatalogue) {
    let mut model = expanded_model();
    let mut selected = &mut model;
    for component in scope.split_whitespace().skip(1) {
        let Some(child) = selected.find_subcommand_mut(component) else {
            return (String::new(), scoped(catalog(&model), scope));
        };
        *child = child.clone().hide(false);
        selected = child;
    }
    let text = selected.render_long_help().to_string();
    let catalogue = scoped(catalog(&model), scope);
    (text, catalogue)
}
// Explicit help can address quota maintenance without advertising it in the
// default catalogue. Only the validated scope is revealed, never its siblings.
fn scoped_catalogue(mut model: clap::Command, scope: &str) -> HelpCatalogue {
    let mut selected = &mut model;
    for component in scope.split_whitespace().skip(1) {
        let Some(child) = selected.find_subcommand_mut(component) else {
            return scoped(catalog(&model), scope);
        };
        *child = child.clone().hide(false);
        selected = child;
    }
    scoped(catalog(&model), scope)
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
fn command_at_path<'a>(root: &'a clap::Command, path: &str) -> Option<&'a clap::Command> {
    path.split_whitespace()
        .skip(1)
        .try_fold(root, |command, part| command.find_subcommand(part))
}
fn append_child_index(mut text: String, scope: &str) -> String {
    let full = expanded_model();
    let Some(command) = command_at_path(&full, scope) else {
        return text;
    };
    let children: Vec<_> = command
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
        .collect();
    if children.is_empty() {
        return text;
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("\nSubcommands:\n");
    for child in children {
        let description = child
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default();
        text.push_str(&format!("  {:<14} {}\n", child.get_name(), description));
    }
    text.push_str(&format!(
        "For focused help, run `provenant help {} SUBCOMMAND`.\n",
        scope.trim_start_matches("provenant ")
    ));
    text
}
fn catalog(model: &clap::Command) -> HelpCatalogue {
    let full = expanded_model();
    fn walk(
        command: &clap::Command,
        path: String,
        commands: &mut Vec<HelpCommand>,
        full: &clap::Command,
    ) {
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
                    required_unless: arg
                        .get::<metadata::RequiredUnless>()
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
        let full_command = command_at_path(full, &path);
        commands.push(HelpCommand {
            command: path.clone(),
            description: command.get_about().map(ToString::to_string),
            subcommands: full_command
                .into_iter()
                .flat_map(|command| command.get_subcommands())
                .filter(|child| !child.is_hide_set())
                .map(|child| child.get_name().to_owned())
                .collect(),
            config_domains: crate::config_reads::domains_for(&path)
                .into_iter()
                .map(str::to_owned)
                .collect(),
            arguments,
            groups,
            constraints: constraints(&path),
        });
        for child in command.get_subcommands() {
            walk(
                child,
                format!("{path} {}", child.get_name()),
                commands,
                full,
            );
        }
    }
    let mut commands = Vec::new();
    walk(model, "provenant".into(), &mut commands, &full);
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
pub fn command_verbs() -> Vec<String> {
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
    let mut verbs = Vec::new();
    walk(&command_model(), String::new(), &mut verbs);
    verbs
}

/// Command paths plus the two non-command events recorded by usage telemetry.
pub fn usage_verbs() -> Vec<String> {
    let mut verbs = vec!["parse".into(), "version".into()];
    verbs.extend(command_verbs());
    verbs
}
const WAIT_JSON_CONTRACT: &str = "With --json, wait writes JSONL: zero or more event objects followed by one terminal command-result envelope. Events have an `event` field; the terminal envelope has `command`, `status` and `data`. Settlement events are emitted without --follow; --follow also emits each observed transition.";

fn constraints(path: &str) -> Vec<String> {
    if path == "provenant output" {
        return vec![
            "target is NAME[/NODE[#N]]; a node reads its newest settled attempt".into(),
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
            "run needs a template; `provenant templates` lists them",
            "--set binds a string parameter as written and any other type as TOML; quote nothing for a string",
            "--file reads a graph document; a plain text brief goes in BRIEF, --brief or --brief-file",
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
            "target is NAME[/NODE[#N]]: a graph recovers, a node starts its next attempt, an attempt continues",
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
            "every unsettled graph is shown; settled graphs from the last 24 hours fill up to 20 rows, with an omission count",
            "--all pages every graph with --limit and --cursor",
        ],
        "provenant wait" => &[
            "every target resolves before the wait starts; no target waits for this project's running graphs",
            "exit 0 success, 1 settled without success, 2 invalid or not found, 5 needs input, 124 timeout, 130 cancelled",
            WAIT_JSON_CONTRACT,
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
    #[test]
    fn run_accepts_writer_branch_base_and_named_route() {
        let invocation = valid(&[
            "run",
            "implement",
            "Fix parser",
            "--mode",
            "worktree-write",
            "--branch",
            "fix/parser",
            "--base",
            "origin/main",
            "--route",
            "senior",
            "--write-scope",
            "src",
        ]);
        let Command::Run(request) = invocation.command else {
            panic!("expected run");
        };
        assert_eq!(request.brief.as_deref(), Some("Fix parser"));
        assert_eq!(request.branch.as_deref(), Some("fix/parser"));
        assert_eq!(request.base.as_deref(), Some("origin/main"));
        assert_eq!(request.template.as_deref(), Some("implement"));
        assert_eq!(request.route.as_deref(), Some("senior"));
        assert_eq!(request.write_scope, vec!["src"]);
        valid(&[
            "run",
            "implement",
            "Fix parser",
            "--worktree",
            ".worktrees/fix-parser",
        ]);
        invalid(&["run", "implement", "Fix parser", "--branch", "fix/parser"]);
        invalid(&["run", "implement", "Fix parser", "--base", "HEAD"]);
        invalid(&[
            "run",
            "implement",
            "Fix parser",
            "--branch",
            "fix/parser",
            "--base",
            "HEAD",
            "--worktree",
            ".",
        ]);
    }

    #[test]
    fn catalogue_bounds_follow_parser_declarations() {
        let model = expanded_model();
        let catalogue = catalog(&model);
        for entry in &catalogue.commands {
            let command = entry
                .command
                .split_whitespace()
                .skip(1)
                .fold(&model, |command, name| {
                    command.find_subcommand(name).unwrap()
                });
            for argument in &entry.arguments {
                let arg = command
                    .get_arguments()
                    .find(|arg| arg.get_id().as_str() == argument.name)
                    .unwrap();
                let numeric = arg.get::<metadata::NumericBounds>().map(|bounds| bounds.0);
                let published = arg
                    .get::<metadata::PublishedBounds>()
                    .map(|bounds| bounds.0);
                let Some(bounds) = numeric.or(published) else {
                    continue;
                };
                assert_eq!(
                    argument.bounds,
                    Some(bounds),
                    "{} {}",
                    entry.command,
                    argument.name
                );
                if numeric.is_some() {
                    let accepts = |value: u64| {
                        clap::Command::new("probe")
                            .arg(Arg::new("value").value_parser(arg.get_value_parser().clone()))
                            .try_get_matches_from(["probe", &value.to_string()])
                            .is_ok()
                    };
                    let (min, max) = bounds;
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
        }

        let stop = model.find_subcommand("stop").unwrap();
        let reason = stop
            .get_arguments()
            .find(|arg| arg.get_id().as_str() == "reason")
            .unwrap();
        let maximum = reason.get::<metadata::PublishedBounds>().unwrap().0.1;
        let unit = "é";
        let unit_bytes = u64::try_from(unit.len()).unwrap();
        assert_eq!(maximum % unit_bytes, 0);
        let exact = unit.repeat(usize::try_from(maximum / unit_bytes).unwrap());
        let over = format!("{exact}{unit}");
        invalid(&["stop", "graph", "--reason", &over]);
        valid(&["stop", "graph", "--reason", &exact]);
    }
    #[test]
    fn observation_wait_catalogue_reports_the_clamped_effective_range() {
        let maximum = provenant_core::MAX_LANE_RUNTIME_SECONDS;
        let catalogue = catalog(&expanded_model());
        let command = catalogue
            .commands
            .iter()
            .find(|entry| entry.command == "provenant fabric events read")
            .unwrap();
        assert_eq!(
            command
                .arguments
                .iter()
                .find(|arg| arg.name == "wait")
                .unwrap()
                .bounds,
            Some((0, maximum))
        );
        assert_eq!(crate::grammar::observation_wait_seconds("0s"), Ok(0));
        assert_eq!(
            crate::grammar::observation_wait_seconds(&format!("{}s", maximum + 1)),
            Ok(maximum)
        );
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
    fn each_public_command_help_exposes_declared_long_arguments() {
        fn collect(command: &clap::Command, path: String, paths: &mut Vec<String>) {
            if command.is_hide_set() {
                return;
            }
            paths.push(path.clone());
            for child in command.get_subcommands() {
                collect(child, format!("{path} {}", child.get_name()), paths);
            }
        }

        let model = expanded_model();
        let mut paths = Vec::new();
        collect(&model, model.get_name().to_owned(), &mut paths);
        for path in paths {
            let args = path
                .split_whitespace()
                .skip(1)
                .chain(std::iter::once("--help"))
                .collect::<Vec<_>>();
            let ParseOutcome::Help { text, .. } = parsed(&args) else {
                panic!("expected help for {path}");
            };
            let command = path
                .split_whitespace()
                .skip(1)
                .fold(&model, |command, name| {
                    command.find_subcommand(name).unwrap()
                });
            for argument in command.get_arguments() {
                let Some(long) = argument.get_long() else {
                    continue;
                };
                let present = text.contains(&format!("--{long}"));
                assert_eq!(present, !argument.is_hide_set(), "{path} --{long}");
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
    fn core_path_help_and_defaults() {
        for words in [&[][..], &["help"][..]] {
            let ParseOutcome::Help { catalog, .. } = parsed(words) else {
                panic!("core help")
            };
            let actual = catalog
                .commands
                .iter()
                .filter_map(|entry| entry.command.strip_prefix("provenant "))
                .filter(|path| !path.contains(' ') && *path != "help")
                .collect::<std::collections::BTreeSet<_>>();
            let marked = command_model()
                .get_subcommands()
                .filter(|command| command.get::<metadata::CoreCommand>().is_some())
                .map(|command| command.get_name().to_owned())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                actual,
                marked
                    .iter()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
            );
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
    fn compact_run_help_keeps_skill_prescribed_options_visible() {
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "run"]) else {
            panic!("run help");
        };
        let run = catalog
            .commands
            .iter()
            .find(|command| command.command == "provenant run")
            .unwrap();
        for flag in ["route", "over", "file"] {
            assert!(
                run.arguments
                    .iter()
                    .any(|argument| argument.long.as_deref() == Some(flag)),
                "run help omits --{flag}"
            );
        }
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "land"]) else {
            panic!("land help");
        };
        let land = catalog
            .commands
            .iter()
            .find(|command| command.command == "provenant land")
            .unwrap();
        for flag in ["head", "shadow", "tier"] {
            assert!(
                land.arguments
                    .iter()
                    .any(|argument| argument.long.as_deref() == Some(flag)),
                "land help omits --{flag}"
            );
        }
    }
    #[test]
    fn explicitly_named_non_core_commands_show_everyday_file_options() {
        for args in [
            vec!["help", "workflow", "session", "save"],
            vec!["workflow", "session", "save", "--help"],
            vec!["help", "tracker", "lint"],
            vec!["tracker", "lint", "--help"],
        ] {
            let ParseOutcome::Help { text, catalog, .. } = parsed(&args) else {
                panic!("help for {args:?}");
            };
            let command_args = if args[0] == "help" {
                &args[1..]
            } else {
                &args[..]
            };
            let path = format!(
                "provenant {}",
                command_args
                    .iter()
                    .take_while(|word| **word != "--help")
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let command = catalog
                .commands
                .iter()
                .find(|command| command.command == path)
                .unwrap_or_else(|| panic!("catalogue omits {path}"));
            assert!(
                command
                    .arguments
                    .iter()
                    .any(|argument| argument.long.as_deref() == Some("file")),
                "{path} catalogue omits --file"
            );
            assert!(text.contains("--file"), "{path} text omits --file: {text}");
        }
    }

    #[test]
    fn graph_run_and_template_controls_parse() {
        let Command::Run(request) = valid(&[
            "run",
            "explore",
            "Inspect the code",
            "--name",
            "inspect",
            "--route",
            "junior",
            "--over",
            "cli",
            "--over",
            "runtime",
            "--set",
            "limit=3",
            "--explain",
            "--detached",
        ])
        .command
        else {
            panic!("run")
        };
        assert_eq!(request.template.as_deref(), Some("explore"));
        assert_eq!(request.brief.as_deref(), Some("Inspect the code"));
        assert_eq!(request.brief_text, None);
        assert_eq!(request.name.as_deref(), Some("inspect"));
        assert_eq!(request.route.as_deref(), Some("junior"));
        assert_eq!(request.over, ["cli", "runtime"]);
        assert_eq!(request.set, ["limit=3"]);
        assert!(request.explain);
        assert!(request.detached);
        valid(&["run", "--file", "work.toml", "--explain"]);
        // A template is the first positional; a document and a brief file each
        // replace the template and brief they stand for, never add to them.
        invalid(&["run"]);
        invalid(&["run", "explore", "--file", "work.toml"]);
        invalid(&["run", "explore", "brief", "--brief-file", "brief.md"]);
        valid(&["run", "explore", "--brief-file", "-"]);
        let Command::Run(named) = valid(&["run", "explore", "--brief", "Named brief"]).command
        else {
            panic!("run")
        };
        assert_eq!(named.brief_text.as_deref(), Some("Named brief"));
        invalid(&["run", "explore", "positional", "--brief", "named"]);
        valid(&[
            "retry",
            "fix-login/implement",
            "--brief",
            "Try the parser again",
        ]);
        valid(&["retry", "fix-login/implement", "--brief-file", "-"]);
        invalid(&[
            "retry",
            "fix-login/implement",
            "--brief",
            "a",
            "--brief-file",
            "b",
        ]);
        invalid(&[
            "retry",
            "fix-login/implement",
            "--brief-file",
            "-",
            "--extend",
            "x=1",
        ]);
        invalid(&["run", "explore", "--name", "Not_A_Name"]);
        valid(&["run", "explore", "--route", "writer=junior,review=senior"]);
        valid(&["config", "schema", "--all"]);
        valid(&[
            "run",
            "task",
            "--model",
            "codex/gpt-6-luna",
            "--effort",
            "high",
            "--account",
            "work",
            "--host",
            "workshop",
        ]);
        invalid(&[
            "run",
            "task",
            "--model",
            "codex/gpt-6-luna",
            "--route",
            "junior",
        ]);
        for flag in [
            "--deliver",
            "--build-heavy",
            "--confidential",
            "--provider-timeout",
        ] {
            invalid(&["run", "task", flag, "example"]);
        }
        valid(&["templates"]);
        valid(&["templates", "review.security", "--file", "inline.toml"]);
        valid(&["edit", "example", "--file", "patch.toml"]);
        invalid(&["edit", "example"]);
        for flag in ["--preset", "--prompt-file", "--manifest", "--plan"] {
            invalid(&["run", "task", flag, "example"]);
        }
    }
    #[test]
    fn help_catalogue_relationships_match_command_declarations() {
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "--all"]) else {
            panic!("help catalogue");
        };
        let model = expanded_model();
        for entry in &catalog.commands {
            let command = entry
                .command
                .split_whitespace()
                .skip(1)
                .fold(&model, |command, name| {
                    command.find_subcommand(name).unwrap()
                });
            for declared in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
                let published = entry
                    .arguments
                    .iter()
                    .find(|arg| arg.name == declared.get_id().as_str())
                    .unwrap_or_else(|| panic!("{} omits {}", entry.command, declared.get_id()));
                let requirements = declared
                    .get::<metadata::Requirements>()
                    .map(|value| value.0.as_slice())
                    .unwrap_or_default();
                let required_unless = declared
                    .get::<metadata::RequiredUnless>()
                    .map(|value| value.0.as_slice())
                    .unwrap_or_default();
                assert_eq!(
                    published
                        .requires
                        .iter()
                        .map(String::as_str)
                        .collect::<std::collections::BTreeSet<_>>(),
                    requirements
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                    "{} {}",
                    entry.command,
                    published.name
                );
                assert_eq!(
                    published
                        .required_unless
                        .iter()
                        .map(String::as_str)
                        .collect::<std::collections::BTreeSet<_>>(),
                    required_unless
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                    "{} {}",
                    entry.command,
                    published.name
                );
            }
        }
        let wait = catalog
            .commands
            .iter()
            .find(|command| command.command == "provenant wait")
            .unwrap();
        assert!(
            wait.constraints
                .iter()
                .any(|constraint| constraint == WAIT_JSON_CONTRACT)
        );
    }

    #[test]
    fn compact_quota_help_indexes_children_from_the_full_grammar() {
        let ParseOutcome::Help { text, catalog, .. } = parsed(&["help", "quota"]) else {
            panic!("quota help");
        };
        let model = expanded_model();
        let expected: std::collections::BTreeSet<_> = model
            .find_subcommand("quota")
            .unwrap()
            .get_subcommands()
            .filter(|child| !child.is_hide_set())
            .map(|child| child.get_name().to_owned())
            .collect();
        for child in &expected {
            assert!(text.contains(child), "missing {child} from {text}");
        }
        let quota = catalog
            .commands
            .iter()
            .find(|c| c.command == "provenant quota")
            .unwrap();
        assert_eq!(
            quota
                .subcommands
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            expected
        );
    }

    #[test]
    fn help_constraints_name_only_options_the_command_has() {
        let ParseOutcome::Help { catalog, .. } = parsed(&["help", "--all"]) else {
            panic!("catalog");
        };
        for command in &catalog.commands {
            for constraint in &command.constraints {
                for flag in constraint
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .filter(|word| word.starts_with("--") && word.len() > 2)
                {
                    let long = flag.trim_start_matches("--");
                    assert!(
                        command
                            .arguments
                            .iter()
                            .any(|a| a.long.as_deref() == Some(long)),
                        "{}: constraint names --{long}, which it lacks: {constraint}",
                        command.command
                    );
                }
            }
        }
    }
    #[test]
    fn retry_key_options_are_optional_but_required_together() {
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
    fn land_accepts_a_pinned_head_and_shows_it_in_default_help() {
        for length in [40, 64] {
            let sha = "a".repeat(length);
            let inv = valid(&["land", "1953", "--head", &sha]);
            let Command::Land { numbers, head, .. } = inv.command else {
                panic!("land command");
            };
            assert_eq!(numbers, [1953]);
            assert_eq!(head, Some(sha));
        }
        // A short or symbolic name parses; landing resolves it against the repository.
        let inv = valid(&["land", "1953", "--head", "abc1234"]);
        let Command::Land { head, .. } = inv.command else {
            panic!("land command");
        };
        assert_eq!(head.as_deref(), Some("abc1234"));
        let mut model = command_model();
        let Some(land) = model.find_subcommand_mut("land") else {
            panic!("land grammar");
        };
        assert!(land.render_help().to_string().contains("--head"));
        let ParseOutcome::Help { text, .. } = parsed(&["help", "land"]) else {
            panic!("default land help");
        };
        assert!(text.contains("--head"), "{text}");
        let ParseOutcome::Help { text, .. } = parsed(&["help", "land", "--all"]) else {
            panic!("all land help");
        };
        assert!(text.contains("--head"), "{text}");
    }
    #[test]
    fn batch_land_and_policy_explanation_parse() -> Result<(), Box<dyn std::error::Error>> {
        let inv = valid(&["land", "12", "34", "--shadow", "--tier", "crucial"]);
        let Command::Land {
            numbers,
            shadow,
            tier,
            ..
        } = inv.command
        else {
            return Err("expected land command".into());
        };
        assert_eq!(numbers, [12, 34]);
        assert!(shadow);
        assert_eq!(tier.as_deref(), Some("crucial"));
        invalid(&["land", "12", "--tier", "high"]);
        valid(&["land", "--explain", "--json"]);
        invalid(&["land", "0", "--shadow"]);
        Ok(())
    }

    #[test]
    fn agent_output_controls_and_land_wait_are_global_and_bounded() {
        valid(&["--full", "quota"]);
        valid(&["route", "list", "--full"]);
        valid(&["land", "2199", "--wait"]);
        valid(&["land", "--wait=30m", "2199"]);
        invalid(&["land", "2199", "--wait=0"]);
        let Command::Land { wait, .. } = valid(&["land", "2199", "--wait=5h"]).command else {
            panic!("land command")
        };
        assert_eq!(wait, Some(provenant_core::MAX_LANE_RUNTIME_SECONDS));
        valid(&["output", "fix-login", "--json"]);
        invalid(&["output", "fix-login", "--summary"]);
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
        let ParseOutcome::Invalid { error, .. } = parsed(words) else {
            panic!("expected invalid invocation: {words:?}");
        };
        assert!(
            matches!(error, CliError::Invalid { .. }),
            "{words:?}: {error:?}"
        );
        assert_eq!(
            exit_status(&provenant_records::Outcome::Invalid),
            2,
            "invalid invocation: {words:?}"
        );
    }
    fn assert_repair_parses(error: &CliError) {
        let repair = error.repair().expect("invalid input has a repair");
        let mut args = Vec::new();
        let mut word = String::new();
        let mut quoted = false;
        let mut started = false;
        let mut chars = repair.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\'' => {
                    quoted = !quoted;
                    started = true;
                }
                '\\' if !quoted => {
                    word.push(chars.next().expect("escaped character"));
                    started = true;
                }
                c if c.is_whitespace() && !quoted => {
                    if started {
                        args.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                c => {
                    word.push(c);
                    started = true;
                }
            }
        }
        assert!(!quoted, "unterminated quote: {repair}");
        if started {
            args.push(word);
        }
        assert_eq!(args.first().map(String::as_str), Some("provenant"));
        assert!(matches!(
            parse(args.into_iter().skip(1).map(OsString::from)),
            ParseOutcome::Invocation(_)
        ));
    }
    #[test]
    fn invalid_invocations_name_the_field_and_offer_parseable_repairs() {
        for (words, expected_field) in [
            (vec!["land", "2199", "--wait=0"], "--wait"),
            (
                vec!["--project", "/path with spaces", "land", "2199", "--wait=0"],
                "--wait",
            ),
            (vec!["land", "--head"], "--head"),
            (vec!["output"], "target"),
            (vec!["stop"], "target"),
            (vec!["retry"], "target"),
            (vec!["steer"], "target"),
            (vec!["edit"], "name"),
            (vec!["route", "resolve"], "route"),
            (vec!["pr", "watch"], "number"),
            (vec!["config"], "subcommand"),
            (vec!["accounts"], "subcommand"),
            (vec!["disk"], "subcommand"),
            (vec!["telemetry"], "subcommand"),
        ] {
            let ParseOutcome::Invalid { error, .. } = parsed(&words) else {
                panic!("expected invalid for {words:?}");
            };
            let text = error.to_string();
            assert!(
                text.to_lowercase().contains(&expected_field.to_lowercase()),
                "{words:?}: expected field {expected_field}, got {text}"
            );
            assert_repair_parses(&error);
            assert_eq!(exit_status(&provenant_records::Outcome::Invalid), 2);
        }
    }

    #[test]
    fn common_usage_errors_offer_parseable_repairs() {
        for words in [
            &["wait", "--timeout", "0"][..],
            &["quota", "history", "--until", "0d"],
            &["route", "resolve", "one", "two"],
            &["output"][..],
            &["land", "2199", "--wait", "5m"],
        ] {
            let ParseOutcome::Invalid { error, .. } = parsed(words) else {
                panic!("expected usage error: {words:?}");
            };
            assert_repair_parses(&error);
        }
    }

    #[test]
    fn misspelt_subcommands_and_flags_offer_parseable_repairs() {
        for words in [&["statsu"][..], &["pr", "statsu"], &["quota", "--fullx"]] {
            let ParseOutcome::Invalid { error, .. } = parsed(words) else {
                panic!("expected usage error: {words:?}");
            };
            assert_repair_parses(&error);
        }
    }

    #[test]
    fn config_without_subcommand_has_a_parseable_repair() {
        let ParseOutcome::Invalid { error, .. } = parsed(&["config"]) else {
            panic!("expected invalid for config without subcommand");
        };
        assert_eq!(error.repair(), Some("provenant config show"));
        assert_repair_parses(&error);
    }

    /// The words that spell `removed`, with a value after a removed flag so
    /// that a flag which took a value is still refused.
    fn removed_words(removed: &Removed) -> Vec<String> {
        let mut words: Vec<String> = removed.words.iter().map(|w| (*w).into()).collect();
        if let Some(flag) = removed.flag {
            words.push(format!("--{flag}"));
            words.push("fix-login".into());
        }
        words
    }

    #[test]
    fn each_removed_spelling_exits_2_with_one_line_naming_its_replacement() {
        for removed in REMOVED
            .iter()
            .filter(|removed| !removed.words.is_empty() || removed.flag.is_some())
        {
            let words = removed_words(removed);
            let ParseOutcome::Invalid { error, .. } = parse(words.iter().map(OsString::from))
            else {
                panic!("{words:?} still parses");
            };
            // Invalid input exits 2, and with no repair the error is one line.
            assert!(matches!(error, CliError::Invalid { .. }), "{words:?}");
            assert_eq!(exit_status(&provenant_records::Outcome::Invalid), 2);
            let text = error.to_string();
            assert_eq!(text.lines().count(), 1, "{words:?}: {text}");
            assert!(text.contains(removed.replacement), "{words:?}: {text}");
            assert_eq!(error.repair(), None, "{words:?}");
        }
    }

    fn target_commands() -> Vec<(Vec<String>, String, super::metadata::TargetSyntax)> {
        fn walk(
            command: &clap::Command,
            words: &[String],
            found: &mut Vec<(Vec<String>, String, super::metadata::TargetSyntax)>,
        ) {
            for argument in command.get_arguments() {
                if argument.is_hide_set() {
                    continue;
                }
                let Some(grammar) = argument.get::<super::metadata::TargetGrammar>() else {
                    continue;
                };
                let flag = argument
                    .get_long()
                    .map(|long| format!("--{long}"))
                    .unwrap_or_default();
                found.push((words.to_vec(), flag, grammar.0));
            }
            for child in command
                .get_subcommands()
                .filter(|child| !child.is_hide_set())
            {
                let mut path = words.to_vec();
                path.push(child.get_name().into());
                walk(child, &path, found);
            }
        }
        let mut found = Vec::new();
        walk(&expanded_model(), &[], &mut found);
        found
    }

    /// Argument words that satisfy a command's other required arguments.
    fn required_extras(words: &[String], flag: &str) -> Vec<&'static str> {
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match (words.as_slice(), flag) {
            (["steer"], _) => vec!["--brief-file", "-"],
            (["edit"], _) => vec!["--file", "patch.json"],
            (["pr", "open"], "--review") => {
                vec!["--from-attempt", "fix-login", "--summary-file", "s.md"]
            }
            (["pr", "open"], _) => vec!["--summary-file", "s.md"],
            (["reconcile"], "--abandon") => vec!["--reason", "superseded"],
            _ => Vec::new(),
        }
    }

    #[test]
    fn every_target_verb_parses_the_generated_target_table_alike() {
        let names = ["a", "fix-login", "g-0123abcd", &"n".repeat(48)];
        let nodes = ["review", "build.v2", "x_y-z", &"N".repeat(200)];
        let mut graph_targets = Vec::new();
        for name in names {
            graph_targets.push(name.to_string());
            for node in nodes {
                graph_targets.push(format!("{name}/{node}"));
                for number in [1, 2, 10, u32::MAX] {
                    graph_targets.push(format!("{name}/{node}#{number}"));
                }
            }
        }
        let graph_refused = [
            "",
            "Fix",
            "1fix",
            "fix-",
            "fix--login",
            "lane:fix",
            "graph:fix",
            "operation:op",
            "land:12",
            "fix/",
            "fix/node#0",
            "fix/node#01",
            "fix/node#",
            "fix#1",
            "fix/a/b",
        ];
        for text in &graph_targets {
            let target: Target = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(target.to_string(), *text);
            assert_eq!(target.to_string().parse::<Target>().ok(), Some(target));
        }
        for text in graph_refused {
            assert!(text.parse::<Target>().is_err(), "{text}");
        }
        let graph_names = names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        let branch_names = ["main", "release/2.0", "feature_x", "@"]
            .map(str::to_owned)
            .to_vec();
        let branch_refused = ["", "-main", "HEAD", "main..next", "release/", "x @{y"];
        let cases = [
            (
                super::metadata::TargetSyntax::Graph,
                graph_targets,
                graph_refused
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
            ),
            (
                super::metadata::TargetSyntax::GraphName,
                graph_names,
                vec!["fix/review".to_owned(), "fix#1".to_owned()],
            ),
            (
                super::metadata::TargetSyntax::Branch,
                branch_names,
                branch_refused
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
            ),
        ];
        let commands = target_commands();
        assert!(
            !commands.is_empty(),
            "command grammar has no target declarations"
        );
        for (words, flag, syntax) in &commands {
            let (accepted_values, refused_values) = cases
                .iter()
                .find(|(kind, _, _)| kind == syntax)
                .map(|(_, accepted, refused)| (accepted, refused))
                .expect("every target declaration has a case table");
            let extras = required_extras(words, flag);
            let mut observed = Vec::new();
            for text in accepted_values
                .iter()
                .chain(refused_values)
                .map(String::as_str)
            {
                let mut argv: Vec<&str> = words.iter().map(String::as_str).collect();
                if !flag.is_empty() {
                    argv.push(flag);
                }
                argv.push(text);
                argv.extend(&extras);
                observed.push(matches!(parsed(&argv), ParseOutcome::Invocation(_)));
            }
            let expected = accepted_values
                .iter()
                .map(|text| match syntax {
                    super::metadata::TargetSyntax::Graph => text.parse::<Target>().is_ok(),
                    super::metadata::TargetSyntax::GraphName => text
                        .parse::<Target>()
                        .is_ok_and(|target| target.node.is_none()),
                    super::metadata::TargetSyntax::Branch => {
                        provenant_confinement::repo::GitRefName::branch(text).is_ok()
                    }
                })
                .chain(refused_values.iter().map(|text| {
                    match syntax {
                        super::metadata::TargetSyntax::Graph => text.parse::<Target>().is_ok(),
                        super::metadata::TargetSyntax::GraphName => text
                            .parse::<Target>()
                            .is_ok_and(|target| target.node.is_none()),
                        super::metadata::TargetSyntax::Branch => {
                            provenant_confinement::repo::GitRefName::branch(text).is_ok()
                        }
                    }
                }))
                .collect::<Vec<_>>();
            assert_eq!(observed, expected, "{words:?} {flag} {syntax:?}");
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
    fn wait_and_output_bounds() {
        valid(&["wait"]);
        valid(&[
            "wait",
            "fix-login",
            "fix-login/review#2",
            "--pr",
            "2721",
            "--any",
        ]);
        valid(&["wait", "--pr", "2721", "--follow", "--timeout", "2m"]);
        for timeout in ["1", "4h", "14400"] {
            valid(&["wait", "fix-login", "--timeout", timeout]);
        }
        for timeout in [
            "14401",
            "5h",
            "99d",
            "18446744073709551615d",
            "999999999999999999999999999999h",
        ] {
            let Command::Wait(wait) = valid(&["wait", "fix-login", "--timeout", timeout]).command
            else {
                panic!("wait command")
            };
            assert_eq!(
                crate::wait::Settings::default().timeout(
                    wait.timeout,
                    &crate::wait::Origin::compiled(Default::default())
                ),
                provenant_core::MAX_LANE_RUNTIME_SECONDS
            );
        }
        for timeout in ["0", "-1", "1.5"] {
            invalid(&["wait", "fix-login", "--timeout", timeout]);
        }
        invalid(&["wait", "--pr", "0"]);
        valid(&["status", "--limit", "100000"]);
        invalid(&["status", "--limit", "0"]);
        let attempt = "fix-login/review#2";
        valid(&["output", attempt, "--tail", "500", "--max-bytes", "65536"]);
        valid(&["output", attempt, "--tail", "501"]);
        invalid(&["output", attempt, "--tail", "0"]);
        invalid(&["output", attempt, "--max-bytes", "65537"]);
        invalid(&["output", attempt, "--offset", "0", "--tail", "1"]);
        valid(&["telemetry", "trace", "--peers", "fix-login"]);
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
    fn control_commands_validate_brief_sources_and_channels() {
        let attempt = "fix-login/implement#1";
        valid(&[
            "steer",
            attempt,
            "--brief",
            "Please continue",
            "--operation-id",
            "s-1",
        ]);
        valid(&["steer", attempt, "--brief-file", "-"]);
        invalid(&["steer", attempt]);
        invalid(&["steer", attempt, "--brief", "a", "--brief-file", "b"]);
        invalid(&[
            "steer",
            "11111111111111111111111111111111",
            "--brief-file",
            "-",
        ]);
        invalid(&["steer", attempt, "--text-stdin"]);
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
        invalid(&["steer", attempt, "--brief-file", "-", "--approve"]);
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
    fn worktree_doctor_requires_and_preserves_a_path() {
        invalid(&["worktree", "doctor"]);
        let invocation = valid(&["worktree", "doctor", "../lane with spaces", "--json"]);
        let Command::Worktree(Worktree::Doctor { path }) = invocation.command else {
            panic!("worktree doctor");
        };
        assert_eq!(path, std::path::PathBuf::from("../lane with spaces"));
    }
    #[test]
    fn collection_caps_are_not_per_occurrence_caps() {
        let mut words = vec!["reconcile".to_string()];
        words.extend((0..100).map(|i| format!("graph-{i}")));
        let argv: Vec<&str> = words.iter().map(String::as_str).collect();
        valid(&argv);
        let mut over = argv.clone();
        over.push("graph-100");
        invalid(&over);
        for (prefix, flag, max) in [(
            vec!["worktree", "remove", "--merged", "--integration", "main"],
            "--branch",
            100,
        )] {
            let id = "a".repeat(32);
            let mut words = prefix;
            for _ in 0..max {
                words.extend([flag, &id]);
            }
            valid(&words);
            words.extend([flag, &id]);
            invalid(&words);
        }
        let reason = "é".repeat(129);
        invalid(&["stop", "fix-login", "--reason", &reason]);
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
    fn global_json_is_recognized_before_or_after_the_command() {
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
            assert_eq!(exit_status(&provenant_records::Outcome::Invalid), 2);
        }
    }

    #[test]
    fn option_separator_keeps_json_out_of_global_transport() {
        assert!(matches!(
            parsed(&["help", "--", "--json"]),
            ParseOutcome::Invalid {
                globals: Globals { json: false, .. },
                ..
            }
        ));
    }

    #[test]
    fn json_help_context_and_command_refusals_follow_the_parser() {
        assert!(matches!(
            parsed(&["hook", "claude", "pre-tool-use", "--help", "--json"]),
            ParseOutcome::Help {
                globals: Globals { json: true, .. },
                ..
            }
        ));
        for words in [
            &["hook", "claude", "pre-tool-use", "--json"][..],
            &["internal", "supervise", "--json"],
            &["-h"],
            &["status", "--lim", "1"],
            &["status", "--limit", "1", "--limit", "2"],
            &["--json", "status", "--json"],
            &["--state-dir", "/a", "status", "--state-dir", "/b"],
            &["status", "--wait"],
            &["fabric", "peer", "--help", "--json"],
            &["help", "nope", "--json"],
        ] {
            invalid(words);
        }
        valid(&["wait", "fix-login"]);
    }

    #[test]
    fn scoped_help_lists_the_declared_command_subtree() {
        fn collect(
            command: &clap::Command,
            path: &str,
            out: &mut std::collections::BTreeSet<String>,
        ) {
            if command.is_hide_set() {
                return;
            }
            out.insert(path.to_owned());
            for child in command.get_subcommands() {
                collect(child, &format!("{path} {}", child.get_name()), out);
            }
        }
        let names = |words: &[&str]| match parsed(words) {
            ParseOutcome::Help { catalog, .. } => catalog
                .commands
                .into_iter()
                .map(|command| command.command)
                .collect::<std::collections::BTreeSet<_>>(),
            other => panic!("{words:?}: {other:?}"),
        };
        let model = expanded_model();
        let mut hosts = std::collections::BTreeSet::new();
        collect(
            model.find_subcommand("hosts").unwrap(),
            "provenant hosts",
            &mut hosts,
        );
        assert_eq!(names(&["help", "hosts", "--json"]), hosts);
        assert_eq!(names(&["hosts", "--help", "doctor", "--json"]), hosts);
        assert_eq!(
            names(&["help", "quota", "history", "--json"]),
            names(&["quota", "history", "--help", "--json"])
        );
        let mail = names(&["help", "fabric", "mail", "--json"]);
        assert!(mail.contains("provenant fabric mail send"));
        assert!(
            mail.iter()
                .all(|command| command.starts_with("provenant fabric mail"))
        );
        assert_eq!(
            names(&["fabric", "mail", "send", "--help", "--json"]),
            std::collections::BTreeSet::from(["provenant fabric mail send".to_owned()])
        );
        assert_eq!(
            names(&["--project", "hosts", "status", "--help", "--json"]),
            std::collections::BTreeSet::from(["provenant status".to_owned()])
        );
        assert_eq!(
            names(&["help", "reconcile", "--json"]),
            std::collections::BTreeSet::from(["provenant reconcile".to_owned()])
        );
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

pub mod steward;
