//! Process edge for local catalogue reads.
use provenant_cli::{Globals, RenderError, RenderMode, console::Console};
use provenant_core::CancelToken;
use provenant_records::{CommandError, CommandResult, Outcome};
use std::io;

#[allow(clippy::disallowed_methods)] // The process edge establishes the real-time deadline.
pub fn run(command: Console, globals: &Globals) -> u8 {
    if matches!(command, Console::Open) {
        return open(globals);
    }
    let leaf = match &command {
        Console::Changes { .. } => "console changes",
        Console::Board { .. } => "console board",
        Console::Tree { .. } => "console tree",
        Console::Overview => "console overview",
        Console::Open => "console open",
        Console::Projects => "console projects",
        Console::Sessions { .. } => "console sessions",
        Console::Detail { .. } => "console detail",
        Console::Search { .. } => "console search",
    };
    let result = (|| -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Optional,
        )?;
        let selected = if globals.project.is_some() {
            use provenant_confinement::{UtilityBudget, UtilityRuntime};
            let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            let runtime = UtilityRuntime::read_only(git)?;
            let executor = provenant_os::exec::SystemExecutor::new(1)?;
            let budget = UtilityBudget::bounded(std::time::Duration::from_secs(5))?;
            let location = runtime
                .project_location(&executor, &resolved.project, budget, &CancelToken::new())?
                .location;
            match provenant_config::lookup_project(&resolved.roots, &location)? {
                Some(binding) => Some(binding.project_id()),
                None => return Err("selected project is not registered".into()),
            }
        } else {
            None
        };
        let local = provenant_cli::console::read(&resolved.roots, selected, &command)?;
        if let Console::Detail { id } = &command
            && local["reason"] == "remote_detail_requires_host_read"
        {
            let (host, attempt) = id.split_once('/').ok_or("invalid remote detail ID")?;
            let context = provenant_cli::fabric::Context::open_read_only(
                resolved.roots,
                &resolved.home,
                &resolved.project,
            )?;
            let Some(peer) = context.peer_host(host) else {
                return Ok(
                    serde_json::json!({"lane_id":{"host":host,"attempt_id":attempt},"status":"unavailable","reason":"unknown_peer"}),
                );
            };
            use provenant_core::Clock;
            let clock = provenant_os::clock::SystemClock::new();
            let seconds = context.config.limits.peer_seconds;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
            let budget = provenant_core::WaitBudget::cli(clock.mono_now(), seconds)?;
            let executor = provenant_os::exec::SystemExecutor::new(1)?;
            let action = provenant_fabric_runtime::hosts::PeerAction::ConsoleDetail {
                extra: Default::default(),
                id: attempt.to_owned(),
            };
            let result = read_peers(vec![peer], deadline, move |host| {
                context.peer_with(
                    &executor,
                    &provenant_os::clock::SystemClock::new(),
                    host,
                    action.clone(),
                    budget,
                    &CancelToken::new(),
                )
            });
            return Ok(compose_detail(
                peer,
                attempt,
                result.into_iter().next().and_then(|(_, result)| result),
            ));
        }
        let (hosts, action) = match &command {
            Console::Board {
                hosts: Some(hosts),
                all,
                limit,
                ..
            } => (
                hosts,
                provenant_fabric_runtime::hosts::PeerAction::ConsoleBoard {
                    extra: Default::default(),
                    all: *all,
                    limit: *limit,
                    selected: selected.is_some(),
                },
            ),
            Console::Tree {
                hosts: Some(hosts),
                max_depth,
                max_nodes,
                detail,
            } => (
                hosts,
                provenant_fabric_runtime::hosts::PeerAction::ConsoleTree {
                    extra: Default::default(),
                    max_depth: *max_depth,
                    max_nodes: *max_nodes,
                    detail: *detail,
                    selected: selected.is_some(),
                },
            ),
            _ => return Ok(local),
        };
        let context = provenant_cli::fabric::Context::open_read_only(
            resolved.roots,
            &resolved.home,
            &resolved.project,
        )?;
        let mut chosen = std::collections::BTreeSet::new();
        if hosts == "all" {
            chosen.extend(context.config.peers.values().map(|peer| peer.host));
        } else {
            for name in hosts.split(',') {
                if name == "local" {
                    continue;
                }
                chosen.insert(context.peer_host(name).ok_or("unknown peer host")?);
            }
        }
        use provenant_core::Clock;
        let clock = provenant_os::clock::SystemClock::new();
        let seconds = context.config.limits.peer_seconds;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        let budget = provenant_core::WaitBudget::cli(clock.mono_now(), seconds)?;
        let executor = provenant_os::exec::SystemExecutor::new(8)?;
        let mut hosts: Vec<_> = chosen.into_iter().collect();
        hosts.sort_by_key(|host| {
            context
                .config
                .peers
                .iter()
                .find_map(|(label, peer)| (peer.host == *host).then_some(label.clone()))
        });
        let results = read_peers(hosts, deadline, move |host| {
            context.peer_with(
                &executor,
                &provenant_os::clock::SystemClock::new(),
                host,
                action.clone(),
                budget,
                &CancelToken::new(),
            )
        });
        compose_peers(local, matches!(command, Console::Board { .. }), results).map_err(Into::into)
    })();
    let result = match result {
        Ok(value) => CommandResult::new(leaf, Outcome::Ok, Some(value)),
        Err(error) => {
            let (status, code) = match error.downcast_ref::<provenant_cli::console::Error>() {
                Some(
                    provenant_cli::console::Error::Cursor
                    | provenant_cli::console::Error::InvalidSession
                    | provenant_cli::console::Error::BoardCursor
                    | provenant_cli::console::Error::SearchCursor
                    | provenant_cli::console::Error::InvalidGenerations,
                ) => (Outcome::Invalid, "CATALOGUE_INPUT"),
                _ => (Outcome::Error, "CATALOGUE"),
            };
            let mut result = CommandResult::new(leaf, status, None);
            result.error = Some(CommandError {
                code: code.into(),
                message: error.to_string(),
                cause: None,
                repair: None,
            });
            result
        }
    };
    let human = if globals.json {
        String::new()
    } else {
        result
            .data
            .as_ref()
            .map(provenant_cli::terse)
            .unwrap_or_default()
    };
    render(
        &result,
        globals,
        &human,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

/// Name of the bundle `apps/quota-menubar/bundle` assembles, which holds the console and the menu agent.
const APP_NAME: &str = "Provenant Console";

/// Opens the console app with the system `open`, or says how to install it.
fn open(globals: &Globals) -> u8 {
    let result = if cfg!(target_os = "macos") {
        let argv: Vec<std::ffi::OsString> = ["/usr/bin/open", "-a", APP_NAME]
            .into_iter()
            .map(Into::into)
            .collect();
        let opened = provenant_os::exec::SystemExecutor::new(1)
            .and_then(|executor| {
                use provenant_os::exec::Executor;
                executor.execute(provenant_os::exec::ExecOptions::new(
                    &argv,
                    std::path::Path::new("/"),
                    std::time::Duration::from_secs(10),
                ))
            })
            .is_ok_and(|outcome| {
                matches!(
                    outcome.termination,
                    provenant_os::exec::Termination::Exit(0)
                )
            });
        if opened {
            CommandResult::new(
                "console open",
                Outcome::Ok,
                Some(serde_json::json!({"app": APP_NAME})),
            )
        } else {
            unavailable(format!(
                "{APP_NAME} is not installed. Build it with apps/quota-menubar/bundle and copy the app to /Applications or ~/Applications."
            ))
        }
    } else {
        unavailable("The console app runs on macOS only.".into())
    };
    let human = match (&result.data, &result.error) {
        (Some(_), _) => format!("Opened {APP_NAME}"),
        (_, Some(error)) => error.message.clone(),
        _ => String::new(),
    };
    render(
        &result,
        globals,
        &human,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

fn unavailable(message: String) -> CommandResult<Option<serde_json::Value>> {
    let mut result = CommandResult::new("console open", Outcome::Error, None);
    result.error = Some(CommandError {
        code: "console_app_unavailable".into(),
        message,
        cause: None,
        repair: None,
    });
    result
}

fn compose_detail(
    host: provenant_core::HostId,
    attempt: &str,
    result: Option<
        Result<provenant_fabric_runtime::hosts::PeerResult, provenant_cli::fabric::PeerFailure>,
    >,
) -> serde_json::Value {
    use provenant_fabric_runtime::hosts::PeerResult;
    match result {
        Some(Ok(PeerResult::Console { mut data, .. }))
            if data["schema_version"] == 1
                && matches!(
                    data["status"].as_str(),
                    Some("found" | "missing" | "unavailable")
                )
                && data["lane_id"]["attempt_id"] == attempt =>
        {
            if let Some(lane) = data
                .get_mut("lane_id")
                .and_then(serde_json::Value::as_object_mut)
            {
                lane.insert("host".into(), serde_json::json!(host));
            }
            data
        }
        _ => {
            serde_json::json!({"schema_version":1,"lane_id":{"host":host,"attempt_id":attempt},"status":"unavailable","reason":"peer_unavailable"})
        }
    }
}

fn compose_peers(
    local: serde_json::Value,
    board: bool,
    results: Vec<(
        provenant_core::HostId,
        Option<
            Result<provenant_fabric_runtime::hosts::PeerResult, provenant_cli::fabric::PeerFailure>,
        >,
    )>,
) -> Result<serde_json::Value, &'static str> {
    use provenant_fabric_runtime::{coordination::facade::FacadeError, hosts::PeerResult};
    let mut boards = Vec::new();
    let mut trees = Vec::new();
    for (host, result) in results {
        let Some(result) = result else {
            if board {
                boards.push(provenant_cli::console::peer::PeerBoard {
                    host,
                    observation: provenant_cli::console::peer::BoardObservation::Unreachable,
                });
            } else {
                trees.push(provenant_cli::console::tree_peer::PeerTree {
                    host,
                    observation: provenant_cli::console::tree_peer::TreeObservation::Unreachable,
                });
            }
            continue;
        };
        let unsupported = matches!(&result, Err(failure) if matches!(failure.error, FacadeError::PeerVersion | FacadeError::PeerProtocolMismatch { .. } | FacadeError::ContinuationUnsupported));
        let unavailable = matches!(&result, Err(failure) if matches!(failure.error, FacadeError::HomeUnavailable | FacadeError::Timeout));
        if board {
            use provenant_cli::console::peer::{BoardObservation as O, PeerBoard};
            let observation = match result {
                Ok(PeerResult::Console { data: page, .. }) => O::Page(page),
                _ if unsupported => O::Unsupported,
                _ if unavailable => O::Unreachable,
                _ => O::Page(serde_json::Value::Null),
            };
            boards.push(PeerBoard { host, observation });
        } else {
            use provenant_cli::console::tree_peer::{PeerTree, TreeObservation as O};
            let observation = match result {
                Ok(PeerResult::Console { data: tree, .. }) => O::Tree(tree),
                _ if unsupported => O::Unsupported,
                _ if unavailable => O::Unreachable,
                _ => O::Tree(serde_json::Value::Null),
            };
            trees.push(PeerTree { host, observation });
        }
    }
    if board {
        provenant_cli::console::peer::compose_board(local, boards)
    } else {
        provenant_cli::console::tree_peer::compose_tree(local, trees)
    }
}

// Detached workers prevent transport cleanup or a late reply from extending the deadline.
#[allow(clippy::disallowed_methods)] // This is the real-time wait boundary for peer reads.
fn read_peers<K, V, F>(hosts: Vec<K>, deadline: std::time::Instant, read: F) -> Vec<(K, Option<V>)>
where
    K: Clone + Send + Sync + 'static,
    V: Send + 'static,
    F: Fn(K) -> V + Send + Sync + 'static,
{
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Instant;

    let hosts = Arc::new(hosts);
    let next = Arc::new(AtomicUsize::new(0));
    let read = Arc::new(read);
    let (sender, receiver) = mpsc::channel();
    for _ in 0..hosts.len().min(8) {
        let (hosts, next, read, sender) =
            (hosts.clone(), next.clone(), read.clone(), sender.clone());
        std::thread::spawn(move || {
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(host) = hosts.get(index) else { break };
                if Instant::now() >= deadline {
                    break;
                }
                let value = read(host.clone());
                let finished = Instant::now();
                if sender.send((index, value, finished)).is_err() {
                    break;
                }
            }
        });
    }
    drop(sender);
    let mut results: Vec<Option<V>> = (0..hosts.len()).map(|_| None).collect();
    while let Ok((index, value, finished)) =
        receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        if finished <= deadline {
            results[index] = Some(value);
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    hosts.iter().cloned().zip(results).collect()
}

// Never publish an overflow artefact, including when rendering errors.
fn render(
    result: &CommandResult<Option<serde_json::Value>>,
    globals: &provenant_cli::Globals,
    human: &str,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> u8 {
    let json = globals.json;
    let mode = globals.render_mode(human, true, None);
    // The result's size is a bound on the result, not on how it is presented:
    // a compact view of an oversized result is still an oversized result.
    let oversized = !json
        && serde_json::to_vec(result)
            .is_ok_and(|bytes| bytes.len() > provenant_cli::MAX_RESULT_BYTES);
    let rendered = if oversized {
        Err(RenderError::TooLarge)
    } else if !json && result.error.is_some() {
        provenant_cli::render(result, mode, stderr)
    } else {
        provenant_cli::render(result, mode, stdout)
    };
    match rendered {
        Ok(()) => provenant_cli::exit_status(&result.status),
        Err(error) => {
            let mut failure = CommandResult::new(&result.command, Outcome::Error, ());
            failure.error = Some(CommandError {
                code: if matches!(error, RenderError::TooLarge) {
                    "RESULT_TOO_LARGE"
                } else {
                    "IO"
                }
                .into(),
                message: if matches!(error, RenderError::TooLarge) {
                    if result.command == "console board" {
                        "board exceeds 1 MiB; reduce --limit. If --limit 1 still fails, the row cannot fit this surface; inspect bounded artefacts with `provenant output ATTEMPT`. No artefact was written".into()
                    } else {
                        "catalogue exceeds 1 MiB; no artefact was written".into()
                    }
                } else {
                    error.to_string()
                },
                cause: None,
                repair: None,
            });
            if json {
                let _ = provenant_cli::render(&failure, RenderMode::Json, stdout);
            } else {
                let _ = provenant_cli::render(
                    &failure,
                    RenderMode::Human {
                        text: "",
                        read_only: true,
                        summary: None,
                    },
                    stderr,
                );
            }
            3
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[allow(clippy::disallowed_methods)] // Exercise real elapsed time and late replies.
    fn slow_peers_share_deadline_and_preserve_local_board() {
        use provenant_core::{AttemptId, HostId, ProjectId};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use std::time::{Duration, Instant};

        let hosts = vec![HostId::from_bytes([2; 16]), HostId::from_bytes([3; 16])];
        let local = serde_json::json!({"lanes":[{
            "lane_id":{"host":HostId::from_bytes([1; 16]),"attempt_id":AttemptId::from_bytes([4; 16])},
            "metadata":{"attempt":{"project_id":ProjectId::from_bytes([5; 16])}}
        }],"next_cursor":"local-cursor","coverage":{"scope":"local"}});
        let start = Instant::now();
        let budget = Duration::from_millis(300);
        let started = Arc::new(AtomicUsize::new(0));
        let counter = started.clone();
        let results = super::read_peers(hosts.clone(), start + budget, move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(650));
            Ok(provenant_fabric_runtime::hosts::PeerResult::Console {
                data: serde_json::Value::Null,
                extra: Default::default(),
            })
        });
        assert_eq!(started.load(Ordering::Relaxed), 2);
        assert!(results.iter().all(|(_, result)| result.is_none()));
        let composed = super::compose_peers(local.clone(), true, results).unwrap();
        assert_eq!(composed["lanes"], local["lanes"]);
        assert_eq!(composed["next_cursor"], local["next_cursor"]);
        for (index, host) in hosts.iter().enumerate() {
            assert_eq!(
                composed["coverage"]["peers"][index]["host"],
                serde_json::json!(host)
            );
            assert_eq!(composed["coverage"]["peers"][index]["state"], "unreachable");
        }
    }

    #[test]
    fn peer_detail_preserves_data_and_reports_unavailable() {
        use provenant_fabric_runtime::hosts::PeerResult;
        let host = provenant_core::HostId::from_bytes([2; 16]);
        let response = PeerResult::Console {
            data: serde_json::json!({"schema_version":1,"lane_id":{"host":null,"attempt_id":"a"},"status":"found","trace":[1]}),
            extra: Default::default(),
        };
        let detail = super::compose_detail(host, "a", Some(Ok(response)));
        assert_eq!(detail["status"], "found");
        assert_eq!(detail["lane_id"]["host"], serde_json::json!(host));
        assert_eq!(detail["trace"], serde_json::json!([1]));
        let missing = super::compose_detail(host, "a", None);
        assert_eq!(missing["status"], "unavailable");
        assert_eq!(missing["lane_id"]["host"], serde_json::json!(host));
        for data in [
            serde_json::json!({"lane_id":{"attempt_id":"a"},"status":"found"}),
            serde_json::json!({"schema_version":2,"lane_id":{"attempt_id":"a"},"status":"found"}),
            serde_json::json!({"schema_version":1,"lane_id":{"attempt_id":"a"},"status":"invalid"}),
        ] {
            let response = PeerResult::Console {
                data,
                extra: Default::default(),
            };
            assert_eq!(
                super::compose_detail(host, "a", Some(Ok(response)))["reason"],
                "peer_unavailable"
            );
        }
    }

    #[test]
    fn oversized_single_board_row_explains_recovery() {
        let value =
            serde_json::json!({"lanes":[{"metadata":"x".repeat(provenant_cli::MAX_RESULT_BYTES)}]});
        let result = provenant_records::CommandResult::new(
            "console board",
            provenant_records::Outcome::Ok,
            Some(value),
        );
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            super::render(
                &result,
                &provenant_cli::Globals {
                    json: true,
                    full: true,
                    ..Default::default()
                },
                "",
                &mut stdout,
                &mut stderr
            ),
            3
        );
        let failure: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(failure["error"]["code"], "RESULT_TOO_LARGE");
        let message = failure["error"]["message"].as_str().unwrap();
        assert!(message.contains("reduce --limit"));
        assert!(message.contains("--limit 1 still fails"));
        assert!(message.contains("provenant output ATTEMPT"));
    }

    #[test]
    fn oversized_human_catalogue_never_publishes() {
        let row = serde_json::json!({"project_id":"x".repeat(32), "common_dir":{"encoding":"utf8","path":"a".repeat(180)}});
        let value = serde_json::json!({"projects":vec![row; 6500], "coverage":{"scope":"local"}});
        let human = provenant_cli::terse(&value);
        assert!(human.len() > provenant_cli::MAX_RESULT_BYTES);
        let result = provenant_records::CommandResult::new(
            "console projects",
            provenant_records::Outcome::Ok,
            Some(value),
        );
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            super::render(
                &result,
                &provenant_cli::Globals {
                    json: false,
                    full: true,
                    ..Default::default()
                },
                &human,
                &mut stdout,
                &mut stderr
            ),
            3
        );
        assert!(stdout.is_empty());
        assert!(
            String::from_utf8(stderr)
                .unwrap()
                .contains("RESULT_TOO_LARGE")
        );
    }
}
