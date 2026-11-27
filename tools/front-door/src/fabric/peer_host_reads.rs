/// One selected destination for a host-scoped read. `host: None` is this
/// process; peer destinations carry their fixed host identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTarget {
    pub name: String,
    pub host: Option<provenant_core::HostId>,
}

/// Resolve the repeatable common host selection once for every read verb.
/// Aliases that name the same peer collapse to one host, and `all` includes
/// the caller before every configured peer.
pub fn selected_hosts(
    local: provenant_core::HostId,
    peers: &provenant_fabric_runtime::config::FabricConfig,
    selection: &provenant_cli::HostSelection,
) -> Result<Vec<HostTarget>, String> {
    let mut hosts = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut add = |name: String, host: Option<provenant_core::HostId>| {
        let key = host.unwrap_or(local);
        if seen.insert(key) {
            hosts.push(HostTarget { name, host });
        }
    };
    match selection {
        provenant_cli::HostSelection::Local => add(local.to_string(), None),
        provenant_cli::HostSelection::Named(name) => {
            resolve_host(name, local, peers, &mut add)?;
        }
        provenant_cli::HostSelection::Many(names) => {
            for name in names {
                resolve_host(name, local, peers, &mut add)?;
            }
        }
        provenant_cli::HostSelection::All => {
            add(local.to_string(), None);
            for (name, peer) in &peers.peers {
                add(name.clone(), Some(peer.host));
            }
        }
    }
    hosts.sort_by(|left, right| {
        (left.host.is_some(), &left.name).cmp(&(right.host.is_some(), &right.name))
    });
    Ok(hosts)
}

fn resolve_host(
    name: &str,
    local: provenant_core::HostId,
    peers: &provenant_fabric_runtime::config::FabricConfig,
    add: &mut impl FnMut(String, Option<provenant_core::HostId>),
) -> Result<(), String> {
    if name == "local" || name == local.to_string() {
        add(local.to_string(), None);
        return Ok(());
    }
    let host = peers
        .peers
        .get(name)
        .map(|peer| peer.host)
        .or_else(|| {
            let parsed = name.parse::<provenant_core::HostId>().ok()?;
            peers
                .peers
                .values()
                .any(|peer| peer.host == parsed)
                .then_some(parsed)
        })
        .ok_or_else(|| format!("unknown peer host {name}"))?;
    add(name.to_owned(), Some(host));
    Ok(())
}

/// One host's contribution to a `--host all` fan-out.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HostReading {
    pub host: String,
    pub status: provenant_records::Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The `data` envelope of an aggregated command: one section per host.
#[derive(Debug, serde::Serialize)]
pub struct HostAggregate {
    pub hosts: Vec<HostReading>,
    /// An unreachable peer accompanied at least one successful host.
    pub partial: bool,
}

/// One host's request outcome before it is folded into the envelope.
#[derive(Debug, Clone)]
pub enum HostResult {
    Ok(serde_json::Value),
    /// A peer answered with the command's own typed outcome.
    Command {
        status: provenant_records::Outcome,
        data: serde_json::Value,
        error: Option<String>,
        warnings: Vec<provenant_records::warning::Warning>,
        next: Vec<provenant_records::command_result::NextStep>,
    },
    Unreachable,
    /// This host delivered earlier pages before a later page became unreachable.
    PartialUnreachable {
        data: serde_json::Value,
        error: String,
    },
    Failed(String),
}

/// Classify a transport failure. Only a refusal or a malformed frame is a
/// host error; every "could not reach the peer" cause is unreachability.
pub fn host_result_from_peer(
    result: Result<serde_json::Value, provenant_cli::fabric::PeerFailure>,
) -> HostResult {
    match result {
        Ok(value) => HostResult::Ok(value),
        Err(failure) => {
            use provenant_fabric_runtime::coordination::facade::FacadeError;
            match failure.error.as_ref() {
                FacadeError::HomeUnavailable
                | FacadeError::PeerVersion
                | FacadeError::PeerProtocolMismatch { .. }
                | FacadeError::Timeout => HostResult::Unreachable,
                error => HostResult::Command {
                    status: host_status_from_facade(error),
                    data: serde_json::Value::Null,
                    error: Some(failure.detail.unwrap_or_else(|| error.to_string())),
                    warnings: Vec::new(),
                    next: Vec::new(),
                },
            }
        }
    }
}

pub fn host_result_from_facade(
    error: provenant_fabric_runtime::coordination::facade::FacadeError,
) -> HostResult {
    let status = host_status_from_facade(&error);
    HostResult::Command {
        status,
        data: serde_json::Value::Null,
        error: Some(error.to_string()),
        warnings: Vec::new(),
        next: Vec::new(),
    }
}

fn host_status_from_facade(
    error: &provenant_fabric_runtime::coordination::facade::FacadeError,
) -> provenant_records::Outcome {
    use provenant_fabric_runtime::coordination::facade::FacadeError;
    match error {
        FacadeError::InvalidInput { .. } | FacadeError::GraphRelayRefused { .. } => {
            provenant_records::Outcome::Invalid
        }
        FacadeError::DispatchRefused { .. }
        | FacadeError::ConfinementRefused { .. }
        | FacadeError::LaneCoordinationRefused
        | FacadeError::CredentialRoot { .. }
        | FacadeError::UpgradeRefused => provenant_records::Outcome::Refused,
        FacadeError::Timeout => provenant_records::Outcome::Timeout,
        FacadeError::Cancelled => provenant_records::Outcome::Cancelled,
        FacadeError::HomeUnavailable
        | FacadeError::PeerVersion
        | FacadeError::PeerProtocolMismatch { .. }
        | FacadeError::Busy => provenant_records::Outcome::Unavailable,
        _ => provenant_records::Outcome::Error,
    }
}

/// Compose one result per host into a single envelope. An unreachable peer
/// adds `PEER_UNREACHABLE` and `partial: true` only when another host answered;
/// an all-down fan-out stays a plain failure. The process status is the worst
/// of every per-host reading, so an unreachable host still exits unavailable.
pub fn aggregate_host_readings(
    command: &str,
    readings: Vec<(String, HostResult)>,
) -> provenant_records::CommandResult<HostAggregate> {
    let mut hosts = Vec::with_capacity(readings.len());
    let mut unreachable = Vec::new();
    let mut answered = false;
    let mut warnings = Vec::new();
    let mut next = Vec::new();
    for (host, reading) in readings {
        match reading {
            HostResult::Ok(data) => {
                answered = true;
                hosts.push(HostReading {
                    host,
                    status: provenant_records::Outcome::Ok,
                    data: Some(data),
                    error: None,
                });
            }
            HostResult::Command {
                status,
                data,
                error,
                warnings: mut host_warnings,
                next: host_next,
            } => {
                answered = true;
                for warning in &mut host_warnings {
                    if warning.subject.is_none() {
                        warning.subject = Some(host.clone());
                    }
                }
                warnings.extend(host_warnings);
                next.extend(host_next);
                hosts.push(HostReading {
                    host,
                    status,
                    data: Some(data),
                    error,
                });
            }
            HostResult::Unreachable => {
                unreachable.push(host.clone());
                hosts.push(HostReading {
                    host,
                    status: provenant_records::Outcome::Unavailable,
                    data: None,
                    error: Some(PEER_UNREACHABLE.as_str().to_owned()),
                });
            }
            HostResult::PartialUnreachable { data, error } => {
                answered = true;
                unreachable.push(host.clone());
                hosts.push(HostReading {
                    host,
                    status: provenant_records::Outcome::Unavailable,
                    data: Some(data),
                    error: Some(error),
                });
            }
            HostResult::Failed(reason) => {
                answered = true;
                hosts.push(HostReading {
                    host,
                    status: provenant_records::Outcome::Error,
                    data: None,
                    error: Some(reason),
                });
            }
        }
    }
    let partial = answered && !unreachable.is_empty();
    let status = hosts
        .iter()
        .map(|reading| reading.status)
        .max_by_key(|status| status.exit_code())
        .unwrap_or(provenant_records::Outcome::Ok);
    let mut result =
        provenant_records::CommandResult::new(command, status, HostAggregate { hosts, partial });
    if partial {
        result
            .warnings
            .push(provenant_records::warning::Warning::new(
                PEER_UNREACHABLE,
                format!(
                    "{} did not answer; showing the hosts that did",
                    unreachable.join(", ")
                ),
                provenant_records::warning::detected_source(PEER_UNREACHABLE),
            ));
    }
    result.warnings.extend(warnings);
    result.next.extend(next);
    result
}

/// One human section per host. Compact output keeps the per-host status line
/// and its body; `--full` is handled by the renderer around this envelope.
pub fn host_sections(aggregate: &HostAggregate) -> String {
    aggregate
        .hosts
        .iter()
        .map(|reading| {
            let body = match (&reading.data, &reading.error) {
                (Some(data), _) => provenant_cli::terse(data).trim_end().to_owned(),
                (None, Some(error)) => error.clone(),
                (None, None) => String::new(),
            };
            format!("{}: {:?} {}", reading.host, reading.status, body)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Emit a fan-out through the one envelope path, so JSON and human output
/// share the host sections, warning and exit code.
pub fn emit_host_aggregate(
    command: &str,
    readings: Vec<(String, HostResult)>,
    globals: &provenant_cli::Globals,
) -> u8 {
    let result = aggregate_host_readings(command, readings);
    let human = host_sections(&result.data);
    crate::emit(&result, globals, &human, true)
}

/// Emit a host aggregate and say whether its complete envelope reached stdout.
/// Event pages request full human details so every acknowledged item is visible
/// even when the caller did not pass `--full`.
pub fn emit_host_aggregate_with_delivery(
    command: &str,
    readings: Vec<(String, HostResult)>,
    globals: &provenant_cli::Globals,
    full_human_details: bool,
) -> (u8, bool) {
    let result = aggregate_host_readings(command, readings);
    let human = if full_human_details {
        host_full_sections(&result.data)
    } else {
        host_sections(&result.data)
    };
    crate::output::emit_with_delivery(&result, globals, &human, true, full_human_details)
}

fn host_full_sections(aggregate: &HostAggregate) -> String {
    aggregate
        .hosts
        .iter()
        .map(|reading| {
            let body = match (&reading.data, &reading.error) {
                (Some(data), _) => {
                    serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string())
                }
                (None, Some(error)) => error.clone(),
                (None, None) => String::new(),
            };
            format!("{}: {:?}\n{}", reading.host, reading.status, body)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

