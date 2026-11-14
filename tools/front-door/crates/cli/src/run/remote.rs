//! Peer reads over the fixed `fabric peer` entrypoint: bounded output pages
//! and trace facts. Routing, confinement and execution stay on the peer.
use super::{Edge, Fail, Status};
use crate::{
    Output, Part,
    fabric::{Context, PeerFailure},
};
use provenant_core::{AttemptId, CancelToken, Clock, HostId, WaitBudget};
use provenant_fabric_runtime::{
    coordination::facade::{
        AttemptTarget, FacadeError, OutputBytes, OutputInput, OutputPart, OutputResult,
    },
    hosts::{PeerAction, PeerResult},
    identity::CrossHostProjectKey,
};
use provenant_os::clock::SystemClock;
use provenant_run as run;
use run::ErrorKind;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

fn io_fail(error: impl std::fmt::Display) -> Fail {
    Fail::new(Status::Error, ErrorKind::Io, error.to_string())
}
fn corrupt() -> Fail {
    Fail::new(
        Status::Error,
        ErrorKind::StateCorrupt,
        "peer reply does not match the request",
    )
}
fn unsupported(what: &str) -> Fail {
    Fail::new(
        Status::Refused,
        ErrorKind::Unsupported,
        format!("{what} is not available for a peer run in this build"),
    )
}
/// Map a peer refusal to the CLI's stable codes. Unknown launches keep their
/// operation ID: replaying the same command recovers the receipt.
/// A transport failure names the peer and cause in the message; the stable
/// code is unchanged.
fn peer_failure_fail(failure: PeerFailure) -> Fail {
    let PeerFailure { error, detail } = failure;
    let message = match detail {
        Some(detail) => format!("{error} ({detail})"),
        None => error.to_string(),
    };
    if matches!(error, FacadeError::LaunchUnknown { .. }) {
        return Fail::new(
            Status::Error,
            ErrorKind::LaunchUnknown,
            format!(
                "{message}; rerun the same `run` with the same --name to recover the \
                 peer's receipt before submitting new work"
            ),
        );
    }
    let (status, kind) = crate::fabric::facade_outcome(&error);
    let projection = error.diagnostic();
    Fail {
        detail: Some(Box::new(error)),
        projection: Some(Box::new(projection)),
        ..Fail::new(status, kind, format!("peer: {message}"))
    }
}

/// One Fabric identity and a bounded call budget per peer request.
struct Peer {
    context: Context,
    clock: SystemClock,
    cancel: CancelToken,
}
impl Peer {
    fn open(edge: &Edge<'_>) -> Result<Self, Fail> {
        let context = Context::open(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Fabric, error.to_string()))?;
        Ok(Self {
            context,
            clock: SystemClock::new(),
            cancel: CancelToken::new(),
        })
    }
    fn project(&self) -> Result<CrossHostProjectKey, Fail> {
        self.context
            .identity
            .federation
            .as_ref()
            .map(|project| project.key.clone())
            .ok_or_else(|| {
                Fail::new(
                    Status::Refused,
                    ErrorKind::Refused,
                    "peer placement needs a Git project inside the home directory",
                )
            })
    }
    fn call(&self, host: HostId, action: PeerAction) -> Result<PeerResult, Fail> {
        self.call_within(host, action, Duration::MAX)
    }
    /// One peer request that takes no longer than `remaining`, the time a wait
    /// or watch has left, nor than the configured peer limit. The SSH child's
    /// termination and reap come out of `remaining` too. Only the first read of
    /// a zero-second wait has nothing left, and it keeps the one-second minimum.
    fn call_within(
        &self,
        host: HostId,
        action: PeerAction,
        remaining: Duration,
    ) -> Result<PeerResult, Fail> {
        let budget = WaitBudget::cli(
            self.clock.mono_now(),
            call_seconds(remaining, self.context.config.limits.peer_seconds),
        )
        .map_err(io_fail)?;
        if remaining.is_zero() || remaining == Duration::MAX {
            return self
                .context
                .peer(&self.clock, host, action, budget, &self.cancel)
                .map_err(peer_failure_fail);
        }
        self.context
            .peer_within(&self.clock, host, action, budget, &self.cancel, remaining)
            .map_err(peer_failure_fail)
    }
}

/// Whole seconds one peer request may take: what is left of the wait, rounded
/// up to the one-second minimum a budget has, and never more than the peer
/// limit.
fn call_seconds(remaining: Duration, peer_seconds: u64) -> u64 {
    remaining
        .as_secs()
        .saturating_add(u64::from(remaining.subsec_nanos() > 0))
        .clamp(1, peer_seconds.max(1))
}

pub(super) fn read_output_with(
    command: &Output,
    host: HostId,
    attempt: AttemptId,
    mut read: impl FnMut(OutputInput) -> Result<OutputResult, Fail>,
) -> Result<(Value, String), Fail> {
    if command.path {
        return Err(unsupported("--path"));
    }
    let part = match command.part {
        Part::Result => OutputPart::Result,
        Part::Stderr => OutputPart::Stderr,
        Part::Prompt => OutputPart::Prompt,
        Part::Events => OutputPart::Events,
        Part::Transcript => return Err(unsupported("--part transcript")),
        Part::Receipt => return Err(unsupported("--part receipt")),
    };
    let chunk = read(OutputInput::Read {
        extra: Default::default(),
        target: AttemptTarget {
            extra: Default::default(),
            attempt,
            host: Some(host),
        },
        part,
        offset: command.offset,
        max_bytes: command.max_bytes,
    })?;
    let fallback = match &chunk {
        OutputResult::Chunk { extra, .. } if matches!(command.part, Part::Result) => extra
            .get(super::output::PLANNER_FALLBACK_KEY)
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    };
    let chunk = if let Some(lines) = command.tail {
        collect_tail(chunk, lines, command.max_bytes, |offset| {
            let input = OutputInput::Read {
                extra: Default::default(),
                target: AttemptTarget {
                    extra: Default::default(),
                    attempt,
                    host: Some(host),
                },
                part,
                offset,
                max_bytes: command.max_bytes,
            };
            read(input)
        })?
    } else {
        chunk
    };
    let (mut data, mut text) = chunk_response(attempt, host, &command.part, chunk)?;
    if let Some(reason) = fallback {
        super::output::add_planner_fallback(&mut data, &mut text, reason);
    }
    Ok((data, text))
}

/// Older peers expose paged reads. Retain only the bounded tail while paging,
/// and refuse a changing artifact rather than combining different digests.
fn collect_tail(
    mut chunk: OutputResult,
    lines: u64,
    max_bytes: u64,
    mut read: impl FnMut(u64) -> Result<OutputResult, Fail>,
) -> Result<OutputResult, Fail> {
    let mut retained = Vec::new();
    let mut retained_events = Vec::new();
    let mut expected_digest = None;
    for _ in 0..1024 {
        let OutputResult::Chunk {
            bytes,
            next_offset,
            eof,
            digest,
            offset,
            part,
            ..
        } = &chunk
        else {
            return Err(corrupt());
        };
        // Event offsets are sequence cursors; their content digest changes per page.
        if !matches!(part, OutputPart::Events) {
            if expected_digest.is_some_and(|previous| previous != *digest) {
                return Err(corrupt());
            }
            expected_digest = Some(*digest);
        }
        let raw = match bytes {
            OutputBytes::Utf8(text) => text.as_bytes().to_vec(),
            OutputBytes::Base64(text) => {
                provenant_fabric_runtime::handoff::decode_base64(text).ok_or_else(corrupt)?
            }
        };
        if matches!(part, OutputPart::Events) {
            let page: Vec<Value> = serde_json::from_slice(&raw).map_err(|_| corrupt())?;
            retained_events.extend(page);
            let excess = retained_events
                .len()
                .saturating_sub(lines.min(super::TAIL_MAX) as usize);
            retained_events.drain(..excess);
            retained = serde_json::to_vec(&retained_events).map_err(io_fail)?;
            while retained.len() as u64 > max_bytes && !retained_events.is_empty() {
                retained_events.remove(0);
                retained = serde_json::to_vec(&retained_events).map_err(io_fail)?;
            }
        } else {
            retained.extend_from_slice(&raw);
            let excess = retained.len().saturating_sub(max_bytes as usize);
            retained.drain(..excess);
        }
        if *eof {
            let start = super::tail_start(&retained, lines.min(super::TAIL_MAX));
            retained.drain(..start);
            while !retained.is_empty() && retained[0] & 0xc0 == 0x80 {
                retained.remove(0);
            }
            let mut result = chunk;
            if let OutputResult::Chunk {
                bytes,
                offset,
                next_offset,
                part,
                ..
            } = &mut result
            {
                if !matches!(part, OutputPart::Events) {
                    *offset = next_offset.saturating_sub(retained.len() as u64);
                }
                *bytes = OutputBytes::from_bytes(retained);
            }
            return Ok(result);
        }
        if next_offset <= offset {
            return Err(corrupt());
        }
        chunk = read(*next_offset)?;
    }
    Err(Fail::new(
        Status::Error,
        ErrorKind::Limit,
        "peer output exceeds the bounded tail scan",
    ))
}

fn chunk_response(
    attempt: AttemptId,
    host: HostId,
    part: &Part,
    chunk: OutputResult,
) -> Result<(Value, String), Fail> {
    let OutputResult::Chunk {
        extra,
        state,
        reason,
        offset,
        next_offset,
        eof,
        digest,
        bytes,
        ..
    } = chunk
    else {
        return Err(corrupt());
    };
    if extra.get("attempt_id").and_then(Value::as_str) != Some(attempt.to_string().as_str()) {
        return Err(corrupt());
    }
    let text = match &bytes {
        OutputBytes::Utf8(text) => text.clone(),
        OutputBytes::Base64(data) => data.clone(),
    };
    let mut data = json!({
        "attempt_id": attempt, "host": host, "state": state, "reason": reason,
        "part": format!("{part:?}").to_lowercase(), "offset": offset,
        "next_offset": next_offset, "length": null, "eof": eof, "clipped": null, "complete": null, "digest": digest, "bytes": bytes,
    });
    if let Some(object) = data.as_object_mut() {
        object.extend(extra);
    }
    Ok((data, text))
}

/// What a trace export learned from peers. A peer that cannot be read is
/// listed with its error, never fatal: the rest of the export still stands.
pub(super) struct PeerFacts {
    pub(super) local: HostId,
    pub(super) names: BTreeMap<HostId, String>,
    pub(super) facts: Vec<provenant_records::AttemptFact>,
    pub(super) hosts: Vec<Value>,
}

/// The terminal attempts of every configured peer, however the run got there,
/// read as `lanes --host` reads them: the peer's newest lanes over the peer
/// channel. `None` when this is not a federated project or no peer is
/// configured.
pub(super) fn peer_facts(
    edge: &Edge<'_>,
    keep: &dyn Fn(Option<&provenant_records::run::WorkContext>) -> bool,
) -> Result<Option<PeerFacts>, Fail> {
    let peer = Peer::open(edge)?;
    // Another project's lanes are not this export's to read.
    if peer.project().is_err() || peer.context.config.peers.is_empty() {
        return Ok(None);
    }
    let names: BTreeMap<HostId, String> = peer
        .context
        .config
        .peers
        .iter()
        .map(|(name, p)| (p.host, name.clone()))
        .collect();
    let (facts, hosts) = collect(&names, keep, |host, action| peer.call(host, action));
    Ok(Some(PeerFacts {
        local: peer.context.identity.host,
        names,
        facts,
        hosts,
    }))
}

/// The most lanes one peer returns; the peer rejects a larger page.
const PEER_LANES: u16 = 100;

/// Ask each host for its lanes and keep the terminal ones `keep` accepts. A host that fails is listed with its error and the rest still count.
fn collect(
    names: &BTreeMap<HostId, String>,
    keep: &dyn Fn(Option<&provenant_records::run::WorkContext>) -> bool,
    mut call: impl FnMut(HostId, PeerAction) -> Result<PeerResult, Fail>,
) -> (Vec<provenant_records::AttemptFact>, Vec<Value>) {
    let (mut facts, mut hosts) = (Vec::new(), Vec::new());
    for (host, name) in names {
        let mut row = json!({ "host": host, "name": name, "attempts": 0 });
        let action = PeerAction::Lanes {
            extra: Default::default(),
            all: true,
            limit: PEER_LANES,
        };
        match call(*host, action) {
            Ok(PeerResult::RunStatuses {
                data: records,
                extra: _,
            }) if records.len() <= usize::from(PEER_LANES) => {
                let before = facts.len();
                facts.extend(
                    records
                        .into_iter()
                        .filter(|r| keep(r.context.as_ref()))
                        .filter_map(|r| run::fact(r).ok()),
                );
                row["attempts"] = json!(facts.len() - before);
            }
            Ok(_) => row["error"] = json!(corrupt().message),
            Err(fail) => row["error"] = json!(fail.message),
        }
        hosts.push(row);
    }
    (facts, hosts)
}
