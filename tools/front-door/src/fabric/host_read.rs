//! Data-returning command reads used by multi-host aggregation.
use crate::fabric::peer::{HostResult, HostTarget};
use crate::fabric::unified;
use provenant_cli::{Globals, Output, Part, Status, Target};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::Clock;
use provenant_fabric_runtime::hosts::{HostReadInput, PeerAction, PeerResult};
use provenant_records::CommandResult;
use provenant_records::Outcome;
use serde_json::Value;
use std::path::PathBuf;

pub(crate) fn bounded_timeout_seconds(
    roots: &provenant_core::Roots,
    common: &provenant_cli::CommonArgs,
    default_seconds: u64,
) -> u64 {
    let requested = common.timeout_seconds().unwrap_or(default_seconds);
    bounded_timeout_request(roots, requested)
}

pub(crate) fn bounded_timeout_request(roots: &provenant_core::Roots, requested: u64) -> u64 {
    match provenant_cli::wait::Settings::resolve(roots) {
        Ok((settings, origin)) => settings.timeout(Some(requested), &origin),
        Err(_) => requested,
    }
}

/// Route explicitly selected hosts through the data seam before the ordinary
/// single-host verb renders. This keeps one envelope path for named and
/// aggregated host reads.
pub(crate) fn route_if_selected<R>(
    command: &provenant_cli::Command,
    dispatch: impl FnOnce(&provenant_cli::Command, provenant_cli::HostSelection) -> R,
) -> Option<R> {
    if !matches!(
        command,
        provenant_cli::Command::Status(_)
            | provenant_cli::Command::Output(_)
            | provenant_cli::Command::Result { .. }
            | provenant_cli::Command::Events { .. }
    ) {
        return None;
    }
    let common = command.common()?;
    let selection = common.target();
    if matches!(selection, provenant_cli::HostSelection::Local) {
        return None;
    }
    Some(dispatch(command, selection))
}

pub(crate) fn route(
    command: &provenant_cli::Command,
    selection: provenant_cli::HostSelection,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let Some((verb, common, input)) = read_request(command, globals) else {
        return crate::fail(
            "host read",
            Outcome::Invalid,
            "HOST_READ",
            "this command has no host-read action",
            "select status, output, result or events",
            globals,
        );
    };

    let resolved = match crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    ) {
        Ok(resolved) => resolved,
        Err(error) => {
            return crate::fail(
                verb,
                Outcome::Error,
                "HOST_READ",
                &error.to_string(),
                crate::REPAIR_IO,
                globals,
            );
        }
    };
    let context = match provenant_cli::fabric::Context::open_read_only(
        resolved.roots,
        &resolved.home,
        &resolved.project,
    ) {
        Ok(context) => context,
        Err(error) => {
            return crate::fail(
                verb,
                Outcome::Error,
                "HOST_READ",
                &error.to_string(),
                crate::REPAIR_IO,
                globals,
            );
        }
    };
    let hosts = match crate::fabric::peer::selected_hosts(
        context.identity.host,
        &context.config,
        &selection,
    ) {
        Ok(hosts) => hosts,
        Err(error) => {
            return crate::fail(
                verb,
                Outcome::Invalid,
                "HOST",
                &error,
                "check the configured peer host name",
                globals,
            );
        }
    };
    let clock = provenant_os::clock::SystemClock::new();
    let seconds = bounded_timeout_seconds(&context.roots, common, 30);
    let budget = match provenant_core::WaitBudget::cli(clock.mono_now(), seconds) {
        Ok(budget) => budget,
        Err(error) => {
            return crate::fail(
                verb,
                Outcome::Invalid,
                "TIMEOUT",
                &error.to_string(),
                "pass a shorter --timeout",
                globals,
            );
        }
    };
    let executor = match provenant_os::exec::SystemExecutor::new(8) {
        Ok(executor) => executor,
        Err(error) => {
            return crate::fail(
                verb,
                Outcome::Error,
                "HOST_READ",
                &error.to_string(),
                crate::REPAIR_IO,
                globals,
            );
        }
    };
    let cancel = provenant_core::CancelToken::new();
    let chair = if matches!(
        &input,
        HostReadInput::Status { mine: true, .. }
            | HostReadInput::Status {
                adoptable: true,
                ..
            }
    ) {
        match provenant_cli::fabric::current_chair(&context) {
            Ok(chair) => Some(chair),
            Err(error) => {
                return crate::fail(
                    verb,
                    Outcome::Error,
                    "HOST_READ",
                    &error.to_string(),
                    crate::REPAIR_IO,
                    globals,
                );
            }
        }
    } else {
        None
    };
    let input = match input {
        HostReadInput::Status {
            targets,
            all,
            limit,
            cursor,
            full,
            mine,
            adoptable,
            custody_session,
            session_stream,
            custody_stream,
            ..
        } => HostReadInput::Status {
            targets,
            all,
            limit,
            cursor,
            full,
            mine,
            adoptable,
            custody_session,
            session_stream,
            custody_stream,
            chair,
        },
        input => input,
    };
    if let HostReadInput::Events {
        from,
        follow,
        consumer,
        ..
    } = &input
    {
        return crate::fabric::host_events::route(crate::fabric::host_events::RouteParams {
            context: &context,
            hosts,
            from: *from,
            follow: *follow,
            consumer: consumer.clone(),
            globals,
            clock: &clock,
            executor: &executor,
            budget,
            cancel: &cancel,
        });
    }
    let final_output = matches!(
        &input,
        HostReadInput::Output {
            final_result: true,
            ..
        }
    );
    let operations = ReadOperations {
        context: &context,
        executor: &executor,
        clock: &clock,
        budget,
        cancel: &cancel,
        globals,
        workers,
    };
    let mut readings = fan_out_host_reads(&input, hosts, |host, input| {
        if final_output {
            collect_final_host(host, input, &operations)
        } else {
            read_host(host, input, &operations)
        }
    });

    if let HostReadInput::Output { to: Some(to), .. } = &input {
        match write_aggregate_output(&mut readings, to) {
            Ok(()) => {}
            Err(error) => {
                return crate::fail(
                    verb,
                    Outcome::Error,
                    "OUTPUT_WRITE",
                    &error.to_string(),
                    "check the output path and try again",
                    globals,
                );
            }
        }
    }
    crate::fabric::peer::emit_host_aggregate(verb, readings, globals)
}

fn read_request<'a>(
    command: &'a provenant_cli::Command,
    globals: &Globals,
) -> Option<(&'static str, &'a provenant_cli::CommonArgs, HostReadInput)> {
    use provenant_cli::Command;
    Some(match command {
        Command::Status(args) => (
            "status",
            &args.common,
            HostReadInput::Status {
                targets: args.targets.iter().map(ToString::to_string).collect(),
                all: args.all,
                limit: args.limit,
                cursor: args.cursor,
                full: globals.full,
                mine: args.mine,
                adoptable: args.adoptable,
                custody_session: args.custody_session.clone(),
                session_stream: args.session_stream,
                custody_stream: args.custody_stream,
                chair: None,
            },
        ),
        Command::Output(args) => (
            "output",
            &args.common,
            HostReadInput::Output {
                target: args.target.to_string(),
                part: output_part_name(args.part).into(),
                offset: args.offset,
                tail: args.tail,
                tail_lines: args.tail_lines,
                max_bytes: args.max_bytes,
                path: args.path,
                final_result: args.final_result,
                to: args
                    .to
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                full: globals.full,
            },
        ),
        Command::Result { common, target } => (
            "result",
            common,
            HostReadInput::Result {
                target: target.to_string(),
                full: globals.full,
            },
        ),
        Command::Events {
            common,
            from,
            follow,
            consumer,
        } => (
            "events",
            common,
            HostReadInput::Events {
                from: *from,
                follow: *follow,
                consumer: consumer.clone(),
                limit: 1,
            },
        ),
        _ => return None,
    })
}

fn fan_out_host_reads(
    input: &HostReadInput,
    hosts: Vec<HostTarget>,
    mut read: impl FnMut(&HostTarget, &HostReadInput) -> HostResult,
) -> Vec<(String, HostResult)> {
    hosts
        .iter()
        .map(|host| (host.name.clone(), read(host, input)))
        .collect()
}

struct ReadOperations<'a> {
    context: &'a provenant_cli::fabric::Context,
    executor: &'a provenant_os::exec::SystemExecutor,
    clock: &'a provenant_os::clock::SystemClock,
    budget: provenant_core::WaitBudget,
    cancel: &'a provenant_core::CancelToken,
    globals: &'a Globals,
    workers: &'static dyn WorkerObserver,
}

fn read_host(
    host: &HostTarget,
    input: &HostReadInput,
    operations: &ReadOperations<'_>,
) -> HostResult {
    if let Some(peer) = host.host {
        match operations.context.peer_with(
            operations.executor,
            operations.clock,
            peer,
            PeerAction::HostRead {
                extra: Default::default(),
                input: input.clone(),
            },
            operations.budget,
            operations.cancel,
        ) {
            Ok(PeerResult::HostRead { data: result, .. }) => result_to_host_result(*result),
            Ok(PeerResult::Error { data, .. }) => facade_to_host_result(data.error),
            Ok(_) => HostResult::Failed("peer returned an unexpected host-read result".into()),
            Err(failure) => crate::fabric::peer::host_result_from_peer(Err(failure)),
        }
    } else {
        execute_local_bounded(
            input,
            operations.globals,
            operations.workers,
            std::time::Duration::from_micros(
                operations
                    .budget
                    .remaining(operations.clock.mono_now())
                    .as_micros(),
            ),
        )
    }
}

fn collect_final_host(
    host: &HostTarget,
    input: &HostReadInput,
    operations: &ReadOperations<'_>,
) -> HostResult {
    let HostReadInput::Output {
        target,
        part,
        max_bytes,
        full,
        ..
    } = input
    else {
        return HostResult::Failed("final result requires an output action".into());
    };
    let parsed_target = Target::parse(target).map_err(|error| error.to_string());
    let parsed_part = parse_part(part).map_err(|error| error.to_string());
    let command = match (parsed_target, parsed_part) {
        (Ok(target), Ok(part)) => Output {
            common: Default::default(),
            target,
            part,
            to: None,
            path: false,
            final_result: true,
            offset: 0,
            tail: None,
            tail_lines: None,
            max_bytes: *max_bytes,
        },
        (Err(error), _) | (_, Err(error)) => {
            return HostResult::Command {
                status: Outcome::Invalid,
                data: Value::Null,
                error: Some(error),
                warnings: Vec::new(),
                next: Vec::new(),
            };
        }
    };
    let mut pages = Vec::new();
    let mut unreachable = false;
    let collected = provenant_cli::run::collect_final(&command, |page| {
        let page_input = HostReadInput::Output {
            target: page.target.to_string(),
            part: output_part_name(page.part).into(),
            offset: page.offset,
            tail: None,
            tail_lines: None,
            max_bytes: page.max_bytes,
            path: false,
            final_result: false,
            to: None,
            full: *full,
        };
        match read_host(host, &page_input, operations) {
            HostResult::Command {
                status: Outcome::Ok,
                data,
                ..
            }
            | HostResult::Ok(data) => {
                pages.push(data.clone());
                Ok((data, String::new()))
            }
            HostResult::Command {
                status,
                data,
                error,
                ..
            } => {
                pages.push(data);
                Err(provenant_cli::run::Fail::new(
                    status,
                    provenant_run::ErrorKind::Fabric,
                    error.unwrap_or_else(|| "host output page failed".into()),
                ))
            }
            HostResult::Unreachable => {
                unreachable = true;
                Err(provenant_cli::run::Fail::new(
                    Outcome::Unavailable,
                    provenant_run::ErrorKind::Fabric,
                    "peer did not answer while reading final output",
                ))
            }
            HostResult::PartialUnreachable { data, error } => {
                unreachable = true;
                pages.push(data);
                Err(provenant_cli::run::Fail::new(
                    Outcome::Unavailable,
                    provenant_run::ErrorKind::Fabric,
                    error,
                ))
            }
            HostResult::Failed(error) => Err(provenant_cli::run::Fail::new(
                Outcome::Error,
                provenant_run::ErrorKind::Fabric,
                error,
            )),
        }
    });
    match collected {
        Ok((data, _)) => result_to_host_result(CommandResult::new("output", Outcome::Ok, data)),
        Err(_) if unreachable && pages.is_empty() => HostResult::Unreachable,
        Err(error) if unreachable => HostResult::PartialUnreachable {
            data: serde_json::json!({"pages": pages}),
            error: error.message,
        },
        Err(error) => HostResult::Command {
            status: error.status,
            data: serde_json::json!({"pages": pages}),
            error: Some(error.message),
            warnings: Vec::new(),
            next: Vec::new(),
        },
    }
}

pub(crate) fn result_to_host_result(
    result: CommandResult<Value>,
) -> crate::fabric::peer::HostResult {
    let error = result.error.map(|error| error.into_payload().message);
    crate::fabric::peer::HostResult::Command {
        status: result.status,
        data: result.data,
        error,
        warnings: result.warnings,
        next: result.next,
    }
}

pub(crate) fn execute_local_bounded(
    input: &HostReadInput,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
    timeout: std::time::Duration,
) -> crate::fabric::peer::HostResult {
    use std::sync::mpsc;
    if timeout.is_zero() {
        return crate::fabric::peer::HostResult::Command {
            status: Outcome::Timeout,
            data: Value::Null,
            error: Some("host read exceeded --timeout".into()),
            warnings: Vec::new(),
            next: Vec::new(),
        };
    }
    let deadline = std::time::Instant::now() + timeout;
    let (send, receive) = mpsc::sync_channel(1);
    let input = input.clone();
    let globals = Globals {
        json: globals.json,
        full: globals.full,
        state_dir: globals.state_dir.clone(),
        project: globals.project.clone(),
    };
    let spawned = std::thread::Builder::new()
        .name("provenant-host-read".into())
        .spawn(move || {
            let result = execute(&input, &globals, workers, deadline).map_err(|error| error.to_string());
            let _ = send.send(result);
        });
    if let Err(error) = spawned {
        return crate::fabric::peer::HostResult::Command {
            status: Outcome::Error,
            data: Value::Null,
            error: Some(format!("could not start bounded host read: {error}")),
            warnings: Vec::new(),
            next: Vec::new(),
        };
    }
    match receive.recv_timeout(timeout) {
        Ok(Ok(result)) => result_to_host_result(result),
        Ok(Err(error)) => crate::fabric::peer::HostResult::Command {
            status: Outcome::Error,
            data: Value::Null,
            error: Some(error),
            warnings: Vec::new(),
            next: Vec::new(),
        },
        Err(mpsc::RecvTimeoutError::Timeout) => crate::fabric::peer::HostResult::Command {
            status: Outcome::Timeout,
            data: Value::Null,
            error: Some("host read exceeded --timeout".into()),
            warnings: Vec::new(),
            next: Vec::new(),
        },
        Err(mpsc::RecvTimeoutError::Disconnected) => crate::fabric::peer::HostResult::Command {
            status: Outcome::Error,
            data: Value::Null,
            error: Some("host read ended without a result".into()),
            warnings: Vec::new(),
            next: Vec::new(),
        },
    }
}

fn facade_to_host_result(
    error: provenant_fabric_runtime::coordination::facade::FacadeError,
) -> crate::fabric::peer::HostResult {
    crate::fabric::peer::host_result_from_facade(error)
}

fn output_part_name(part: Part) -> &'static str {
    match part {
        Part::Result => "result",
        Part::Prompt => "prompt",
        Part::Stderr => "stderr",
        Part::Events => "events",
        Part::Receipt => "receipt",
        Part::Transcript => "transcript",
    }
}

fn write_aggregate_output(
    readings: &mut [(String, crate::fabric::peer::HostResult)],
    to: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut combined = Vec::new();
    let mut successful = Vec::new();
    let multiple = readings.len() > 1;
    for (index, (host, reading)) in readings.iter_mut().enumerate() {
        let crate::fabric::peer::HostResult::Command {
            status,
            data,
            error,
            ..
        } = reading
        else {
            continue;
        };
        if *status != Outcome::Ok {
            continue;
        }
        let bytes = match output_data_bytes(data) {
            Ok(bytes) => bytes,
            Err(failure) => {
                *status = Outcome::Invalid;
                *error = Some(failure.to_string());
                continue;
            }
        };
        if multiple {
            combined.extend_from_slice(format!("--- {host} ---\n").as_bytes());
        }
        combined.extend_from_slice(&bytes);
        successful.push(index);
    }
    if successful.is_empty() {
        return Ok(());
    }
    let path = PathBuf::from(to);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let structured =
        std::fs::write(&absolute, &combined).and_then(|()| std::fs::canonicalize(&absolute))?;
    for index in successful {
        let (_, reading) = &mut readings[index];
        let crate::fabric::peer::HostResult::Command { status, data, .. } = reading else {
            continue;
        };
        if *status != Outcome::Ok {
            continue;
        }
        data["to"] = serde_json::to_value(provenant_records::StructuredPathWire::from_path(
            &structured,
        )?)?;
        data["written"] = serde_json::json!(combined.len());
        if let Some(object) = data.as_object_mut() {
            object.remove("bytes");
        }
    }
    Ok(())
}

fn output_data_bytes(data: &Value) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let bytes = &data["bytes"];
    match (bytes["encoding"].as_str(), bytes["data"].as_str()) {
        (Some("utf8"), Some(text)) => Ok(text.as_bytes().to_vec()),
        (Some("base64"), Some(text)) => provenant_fabric_runtime::handoff::decode_base64(text)
            .ok_or_else(|| "invalid output encoding".into()),
        _ => Err(
            "--to needs a part that has bytes; select result, transcript, stderr or prompt".into(),
        ),
    }
}

/// Run one host read. `deadline` bounds the local output reader the same way
/// the invocation timeout bounds the command.
pub(crate) fn execute(
    input: &HostReadInput,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
    deadline: std::time::Instant,
) -> Result<CommandResult<Value>, Box<dyn std::error::Error>> {
    match input {
        HostReadInput::Status {
            targets,
            all,
            limit,
            cursor,
            full,
            mine,
            adoptable,
            custody_session,
            session_stream,
            custody_stream,
            chair,
        } => {
            let targets = targets
                .iter()
                .map(|target| Target::parse(target))
                .collect::<Result<Vec<_>, _>>()?;
            let command = Status {
                common: Default::default(),
                mine: *mine,
                adoptable: *adoptable,
                custody_session: custody_session.clone(),
                session_stream: *session_stream,
                custody_stream: *custody_stream,
                targets,
                all: *all,
                limit: *limit,
                cursor: *cursor,
            };
            let globals = globals_with_full(globals, *full);
            if *session_stream {
                let (mut reply, stream) = with_edge_and_stream(&globals, |edge, out| {
                    provenant_cli::run::status_session_stream(command.clone(), edge, out)
                })?;
                reply.result.data["stream"] = session_stream_rows(&stream)?;
                return Ok(reply.result);
            }
            if *mine || *adoptable {
                return crate::custody::status_data(&command, &globals, chair.clone());
            }
            let data = unified::host_status_data(&command, &globals, workers)?;
            Ok(CommandResult::new("status", Outcome::Ok, data))
        }
        HostReadInput::Output {
            target,
            part,
            offset,
            tail,
            tail_lines,
            max_bytes,
            path,
            final_result,
            to: _,
            full,
        } => {
            let command = Output {
                common: Default::default(),
                target: Target::parse(target)?,
                part: parse_part(part)?,
                to: None,
                path: *path,
                final_result: *final_result,
                offset: *offset,
                tail: *tail,
                tail_lines: *tail_lines,
                max_bytes: *max_bytes,
            };
            let globals = globals_with_full(globals, *full);
            let reply = with_edge(&globals, |edge| {
                unified::unified_graph_output(&command, edge, &globals, workers, deadline)
            })?;
            Ok(reply.result)
        }
        HostReadInput::Result { target, full } => {
            let target = Target::parse(target)?;
            let globals = globals_with_full(globals, *full);
            let reply = with_edge(&globals, |edge| {
                unified::unified_graph_result(&target, edge, std::time::Duration::from_secs(55))
            })?;
            Ok(reply.result)
        }
        HostReadInput::Events {
            from,
            follow: _,
            consumer,
            limit,
        } => {
            let limit = usize::try_from(*limit)?;
            let page = crate::events::read_page(*from, limit, consumer.as_deref(), globals)?;
            let mut data = page.data;
            data["ack_cursor"] = page
                .ack_cursor
                .map_or(Value::Null, |cursor| serde_json::json!(cursor));
            Ok(CommandResult::new("events", Outcome::Ok, data))
        }
        HostReadInput::EventsAck {
            consumer,
            through_cursor,
        } => {
            crate::events::acknowledge_page(consumer, *through_cursor, globals)?;
            Ok(CommandResult::new(
                "events",
                Outcome::Ok,
                serde_json::json!({"acknowledged":true}),
            ))
        }
    }
}

fn session_stream_rows(stream: &[u8]) -> Result<Value, serde_json::Error> {
    stream
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<Value>, _>>()
        .map(Value::Array)
}

fn with_edge(
    globals: &Globals,
    action: impl FnOnce(&provenant_cli::run::Edge<'_>) -> provenant_cli::run::Reply,
) -> Result<provenant_cli::run::Reply, Box<dyn std::error::Error>> {
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )?;
    let edge = provenant_cli::run::Edge {
        roots: &resolved.roots,
        home: &resolved.home,
        assets: &resolved.assets,
        cwd: &resolved.project,
    };
    Ok(action(&edge))
}

fn with_edge_and_stream(
    globals: &Globals,
    action: impl FnOnce(&provenant_cli::run::Edge<'_>, &mut Vec<u8>) -> provenant_cli::run::Reply,
) -> Result<(provenant_cli::run::Reply, Vec<u8>), Box<dyn std::error::Error>> {
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )?;
    let edge = provenant_cli::run::Edge {
        roots: &resolved.roots,
        home: &resolved.home,
        assets: &resolved.assets,
        cwd: &resolved.project,
    };
    let mut stream = Vec::new();
    let reply = action(&edge, &mut stream);
    Ok((reply, stream))
}

fn globals_with_full(globals: &Globals, full: bool) -> Globals {
    Globals {
        json: true,
        full,
        state_dir: globals.state_dir.clone(),
        project: globals.project.clone(),
    }
}

fn parse_part(part: &str) -> Result<Part, std::io::Error> {
    match part {
        "result" => Ok(Part::Result),
        "prompt" => Ok(Part::Prompt),
        "stderr" => Ok(Part::Stderr),
        "events" => Ok(Part::Events),
        "receipt" => Ok(Part::Receipt),
        "transcript" => Ok(Part::Transcript),
        _ => Err(std::io::Error::other("invalid output part")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::ffi::OsString;

    #[test]
    fn session_stream_preserves_json_rows_and_rejects_invalid_rows() {
        let rows = session_stream_rows(b"{\"id\":1}\n\n{\"id\":2}\n").unwrap();
        assert_eq!(rows, json!([{ "id": 1 }, { "id": 2 }]));
        assert!(session_stream_rows(b"{invalid}\n").is_err());
    }

    #[test]
    fn parsed_status_all_dispatches_to_per_host_aggregation() {
        let provenant_cli::ParseOutcome::Invocation(invocation) = provenant_cli::parse(
            ["status", "--host", "all", "--all", "--limit", "7"].map(OsString::from),
        ) else {
            panic!("valid status --host all invocation did not parse");
        };

        let hosts = vec![
            HostTarget {
                name: "alpha".into(),
                host: None,
            },
            HostTarget {
                name: "beta".into(),
                host: None,
            },
        ];
        let Some(result) = route_if_selected(&invocation.command, |command, selection| {
            assert_eq!(selection, provenant_cli::HostSelection::All);
            let Some((verb, _common, input)) = read_request(command, &invocation.globals) else {
                panic!("selected status did not produce its typed read request");
            };
            assert_eq!(verb, "status");
            let HostReadInput::Status {
                targets,
                all,
                limit,
                ..
            } = &input
            else {
                panic!("status dispatch changed the typed host read");
            };
            assert!(targets.is_empty());
            assert!(*all);
            assert_eq!(*limit, 7);

            let readings = fan_out_host_reads(&input, hosts, |host, input| {
                let HostReadInput::Status {
                    targets,
                    all,
                    limit,
                    ..
                } = input
                else {
                    panic!("status host read changed verb");
                };
                assert!(targets.is_empty());
                assert!(*all);
                assert_eq!(*limit, 7);
                HostResult::Ok(json!({"host": host.name, "all": all, "limit": limit}))
            });
            crate::fabric::peer::aggregate_host_readings(verb, readings)
        }) else {
            panic!("explicit status host selection was not dispatched");
        };

        assert_eq!(result.status, Outcome::Ok);
        assert!(!result.data.partial);
        assert_eq!(result.data.hosts.len(), 2);
        assert_eq!(result.data.hosts[0].host, "alpha");
        assert!(result.data.hosts[0].data.as_ref().is_some_and(|data| {
            data["all"].as_bool() == Some(true) && data["limit"].as_u64() == Some(7)
        }));
        assert_eq!(result.data.hosts[1].host, "beta");
    }
}
