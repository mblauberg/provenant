//! Bounded, delivery-aware event reads across selected hosts.
use crate::fabric::peer::{HostResult, HostTarget};
use provenant_cli::Globals;
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use provenant_fabric_runtime::hosts::{HostReadInput, PeerAction, PeerResult};
use provenant_os::exec::Executor;
use provenant_records::Outcome;
use serde_json::{Value, json};

const PAGE_LIMIT: u64 = 1;
const FOLLOW_POLL_MICROS: u64 = 250_000;

struct HostCursor {
    target: HostTarget,
    cursor: Option<i64>,
    active: bool,
    delivered_rows: u64,
    last_reading: HostResult,
}

struct PendingAck {
    target: HostTarget,
    cursor: i64,
}

struct HostOperations<'a> {
    context: &'a provenant_cli::fabric::Context,
    globals: &'a Globals,
    clock: &'a dyn Clock,
    executor: &'a dyn Executor,
    budget: WaitBudget,
    cancel: &'a CancelToken,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
}

impl HostOperations<'_> {
    fn read(&self, host: &HostCursor, input: &HostReadInput) -> HostResult {
        match host.target.host {
            Some(peer) => match self.context.peer_with(
                self.executor,
                self.clock,
                peer,
                PeerAction::HostRead {
                    extra: Default::default(),
                    input: input.clone(),
                },
                self.budget,
                self.cancel,
            ) {
                Ok(PeerResult::HostRead { data: result, .. }) => {
                    crate::fabric::host_read::result_to_host_result(*result)
                }
                Ok(PeerResult::Error { data, .. }) => {
                    crate::fabric::peer::host_result_from_facade(data.error)
                }
                Ok(_) => HostResult::Failed("peer returned an unexpected events result".into()),
                Err(failure) => crate::fabric::peer::host_result_from_peer(Err(failure)),
            },
            None => crate::fabric::host_read::execute_local_bounded(
                input,
                self.globals,
                self.workers,
                std::time::Duration::from_micros(
                    self.budget.remaining(self.clock.mono_now()).as_micros(),
                ),
            ),
        }
    }

    fn acknowledge(
        &self,
        host: &HostTarget,
        consumer: &str,
        through_cursor: i64,
    ) -> Result<(), String> {
        let input = HostReadInput::EventsAck {
            consumer: consumer.to_owned(),
            through_cursor,
        };
        let result = match host.host {
            Some(peer) => match self.context.peer_with(
                self.executor,
                self.clock,
                peer,
                PeerAction::HostRead {
                    extra: Default::default(),
                    input,
                },
                self.budget,
                self.cancel,
            ) {
                Ok(PeerResult::HostRead { data: result, .. }) => {
                    crate::fabric::host_read::result_to_host_result(*result)
                }
                Ok(PeerResult::Error { data, .. }) => {
                    crate::fabric::peer::host_result_from_facade(data.error)
                }
                Ok(_) => {
                    HostResult::Failed("peer returned an unexpected events acknowledgement".into())
                }
                Err(failure) => crate::fabric::peer::host_result_from_peer(Err(failure)),
            },
            None => crate::fabric::host_read::execute_local_bounded(
                &input,
                self.globals,
                self.workers,
                std::time::Duration::from_micros(
                    self.budget.remaining(self.clock.mono_now()).as_micros(),
                ),
            ),
        };
        match result {
            HostResult::Command {
                status: Outcome::Ok,
                error: None,
                ..
            } => Ok(()),
            HostResult::Command { status, error, .. } => Err(format!(
                "could not acknowledge events on {} ({status:?}): {}",
                host.name,
                error.unwrap_or_else(|| "host returned an unsuccessful acknowledgement".into())
            )),
            HostResult::Unreachable => Err(format!(
                "could not acknowledge events on {}: peer is unreachable",
                host.name
            )),
            HostResult::PartialUnreachable { error, .. } | HostResult::Failed(error) => Err(
                format!("could not acknowledge events on {}: {error}", host.name),
            ),
            HostResult::Ok(_) => Ok(()),
        }
    }
}

/// Read one event at a time from each host. Each published envelope is a
/// complete delivery unit, so consumer cursors advance only after that whole
/// envelope reached stdout.
pub(crate) struct RouteParams<'a> {
    pub(crate) context: &'a provenant_cli::fabric::Context,
    pub(crate) hosts: Vec<HostTarget>,
    pub(crate) from: Option<i64>,
    pub(crate) follow: bool,
    pub(crate) consumer: Option<String>,
    pub(crate) globals: &'a Globals,
    pub(crate) clock: &'a dyn Clock,
    pub(crate) executor: &'a dyn Executor,
    pub(crate) budget: WaitBudget,
    pub(crate) cancel: &'a CancelToken,
}

pub(crate) fn route(params: RouteParams<'_>) -> u8 {
    let RouteParams {
        context,
        hosts,
        from,
        follow,
        consumer,
        globals,
        clock,
        executor,
        budget,
        cancel,
    } = params;
    static WORKERS: provenant_confinement::worktrees::SystemWorkerObserver =
        provenant_confinement::worktrees::SystemWorkerObserver;
    let operations = HostOperations {
        context,
        globals,
        clock,
        executor,
        budget,
        cancel,
        workers: &WORKERS,
    };

    let mut cursors: Vec<_> = hosts
        .into_iter()
        .map(|target| HostCursor {
            target,
            cursor: from,
            active: true,
            delivered_rows: 0,
            last_reading: command_result(
                Outcome::Ok,
                json!({
                    "items": [],
                    "next_cursor": from.unwrap_or_default(),
                    "truncated": false,
                }),
                None,
            ),
        })
        .collect();
    let mut first_page = true;
    let mut worst_exit = 0u8;

    loop {
        if cancel.is_cancelled() {
            let status = crate::fail(
                "events",
                Outcome::Cancelled,
                "CANCELLED",
                "event delivery was cancelled",
                "rerun `provenant events` to resume from the consumer cursor",
                globals,
            );
            return worst_exit.max(status);
        }

        if follow && budget.remaining(clock.mono_now()).as_micros() == 0 {
            let readings: Vec<_> = cursors
                .iter_mut()
                .map(|host| {
                    if host.active {
                        host.active = false;
                        host.last_reading = command_result(
                            Outcome::Timeout,
                            Value::Null,
                            Some("event follow exceeded --timeout".into()),
                        );
                    }
                    (host.target.name.clone(), host.last_reading.clone())
                })
                .collect();
            if readings.is_empty() {
                return worst_exit.max(Outcome::Timeout.exit_code());
            }
            return emit_page(readings, globals, worst_exit);
        }

        let mut readings = Vec::with_capacity(cursors.len());
        let mut acknowledgements = Vec::new();
        let mut delivered_hosts = Vec::new();
        let mut has_events = false;
        let mut has_failure = false;

        for host in &mut cursors {
            if !host.active {
                readings.push((host.target.name.clone(), host.last_reading.clone()));
                continue;
            }
            let input = HostReadInput::Events {
                from: host.cursor,
                follow: false,
                consumer: consumer.clone(),
                limit: PAGE_LIMIT,
            };
            let reading = operations.read(host, &input);

            match reading {
                HostResult::Command {
                    status: Outcome::Ok,
                    mut data,
                    error: None,
                    warnings,
                    next,
                } => {
                    let ack_cursor = data.get("ack_cursor").and_then(Value::as_i64);
                    if let Some(object) = data.as_object_mut() {
                        object.remove("ack_cursor");
                    }
                    let items = data.get("items").and_then(Value::as_array);
                    let next_cursor = data.get("next_cursor").and_then(Value::as_i64);
                    let truncated = data
                        .get("truncated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);

                    let Some(items) = items else {
                        host.active = false;
                        has_failure = true;
                        push_reading(
                            host,
                            &mut readings,
                            command_result(
                                Outcome::Error,
                                data,
                                Some("host returned an invalid events page".into()),
                            ),
                        );
                        continue;
                    };
                    let Some(next_cursor) = next_cursor else {
                        host.active = false;
                        has_failure = true;
                        push_reading(
                            host,
                            &mut readings,
                            command_result(
                                Outcome::Error,
                                data,
                                Some("host returned an events page without next_cursor".into()),
                            ),
                        );
                        continue;
                    };
                    let item_count = items.len();
                    let current_cursor = host.cursor;
                    if item_count > 0 && current_cursor.is_some_and(|cursor| next_cursor <= cursor)
                    {
                        host.active = false;
                        has_failure = true;
                        push_reading(
                            host,
                            &mut readings,
                            command_result(
                                Outcome::Error,
                                data,
                                Some(
                                    "host returned an events page without advancing its cursor"
                                        .into(),
                                ),
                            ),
                        );
                        continue;
                    }
                    if item_count > 0 && consumer.is_some() && ack_cursor != Some(next_cursor) {
                        host.active = false;
                        has_failure = true;
                        push_reading(
                            host,
                            &mut readings,
                            command_result(
                                Outcome::Error,
                                data,
                                Some(
                                    "host returned an events page without a matching acknowledgement cursor"
                                        .into(),
                                ),
                            ),
                        );
                        continue;
                    }
                    if item_count == 0 && truncated {
                        host.active = false;
                        has_failure = true;
                        push_reading(
                            host,
                            &mut readings,
                            command_result(
                                Outcome::Error,
                                data,
                                Some("host returned a truncated events page without rows".into()),
                            ),
                        );
                        continue;
                    }

                    has_events |= item_count > 0;
                    host.cursor = Some(next_cursor);
                    host.active = if follow {
                        true
                    } else {
                        item_count > 0 && truncated
                    };
                    if item_count > 0 {
                        delivered_hosts.push((host.target.clone(), item_count as u64));
                    }
                    if item_count > 0
                        && consumer.is_some()
                        && let Some(cursor) = ack_cursor
                    {
                        acknowledgements.push(PendingAck {
                            target: host.target.clone(),
                            cursor,
                        });
                    }
                    push_reading(
                        host,
                        &mut readings,
                        HostResult::Command {
                            status: Outcome::Ok,
                            data,
                            error: None,
                            warnings,
                            next,
                        },
                    );
                }
                HostResult::Command {
                    status,
                    mut data,
                    error,
                    warnings,
                    next,
                } => {
                    if let Some(object) = data.as_object_mut() {
                        object.remove("ack_cursor");
                    }
                    host.active = false;
                    has_failure = true;
                    push_reading(
                        host,
                        &mut readings,
                        HostResult::Command {
                            status,
                            data,
                            error,
                            warnings,
                            next,
                        },
                    );
                }
                HostResult::Unreachable if host.delivered_rows > 0 => {
                    host.active = false;
                    has_failure = true;
                    push_reading(
                        host,
                        &mut readings,
                        HostResult::PartialUnreachable {
                            data: json!({
                                "items": [],
                                "next_cursor": host.cursor.unwrap_or_default(),
                                "delivered_rows": host.delivered_rows,
                            }),
                            error: "peer became unreachable after delivering earlier event pages"
                                .into(),
                        },
                    );
                }
                HostResult::Unreachable => {
                    host.active = false;
                    has_failure = true;
                    push_reading(host, &mut readings, HostResult::Unreachable);
                }
                other => {
                    host.active = false;
                    has_failure = true;
                    push_reading(host, &mut readings, other);
                }
            }
        }

        let should_emit = has_events || has_failure || (first_page && !follow);
        if should_emit {
            let (status, delivered) = emit_page_with_delivery(readings, globals);
            worst_exit = worst_exit.max(status);
            if !delivered {
                return worst_exit.max(3);
            }
            for (target, rows) in delivered_hosts {
                if let Some(host) = cursors.iter_mut().find(|host| host.target == target) {
                    host.delivered_rows = host.delivered_rows.saturating_add(rows);
                }
            }
            for acknowledgement in acknowledgements {
                let Some(consumer) = consumer.as_deref() else {
                    continue;
                };
                match operations.acknowledge(
                    &acknowledgement.target,
                    consumer,
                    acknowledgement.cursor,
                ) {
                    Ok(()) => {}
                    Err(error) => {
                        let status = crate::fail(
                            "events",
                            Outcome::Error,
                            "EVENTS_ACK",
                            &error,
                            "rerun `provenant events --consumer NAME` to redeliver this page",
                            globals,
                        );
                        return worst_exit.max(status);
                    }
                }
            }
        }

        first_page = false;
        let any_active = cursors.iter().any(|host| host.active);
        if !follow && !any_active {
            return worst_exit;
        }
        if follow && !any_active {
            return worst_exit;
        }
        if follow
            && !has_events
            && !has_failure
            && let Err(error) = wait_for_next(clock, budget, cancel)
        {
            let status = crate::fail(
                "events",
                Outcome::Error,
                "EVENTS_WAIT",
                &error,
                "check the monotonic clock and try again",
                globals,
            );
            return worst_exit.max(status);
        }
    }
}

fn push_reading(
    host: &mut HostCursor,
    readings: &mut Vec<(String, HostResult)>,
    reading: HostResult,
) {
    host.last_reading = reading.clone();
    readings.push((host.target.name.clone(), reading));
}

fn command_result(status: Outcome, data: Value, error: Option<String>) -> HostResult {
    HostResult::Command {
        status,
        data,
        error,
        warnings: Vec::new(),
        next: Vec::new(),
    }
}

fn emit_page_with_delivery(readings: Vec<(String, HostResult)>, globals: &Globals) -> (u8, bool) {
    crate::fabric::peer::emit_host_aggregate_with_delivery("events", readings, globals, true)
}

fn emit_page(readings: Vec<(String, HostResult)>, globals: &Globals, previous: u8) -> u8 {
    let result = crate::fabric::peer::aggregate_host_readings("events", readings);
    let status = result.status.exit_code();
    let human = crate::fabric::peer::host_sections(&result.data);
    let emitted = crate::emit(&result, globals, &human, true);
    previous.max(status).max(emitted)
}

fn wait_for_next(
    clock: &dyn Clock,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("event delivery was cancelled".into());
    }
    let now = clock.mono_now();
    let remaining = budget.remaining(now).as_micros();
    if remaining == 0 {
        return Ok(());
    }
    let delay = DurationMicros::from_micros(remaining.min(FOLLOW_POLL_MICROS));
    let deadline = now
        .checked_add(delay)
        .map_err(|error| error.to_string())?
        .min(budget.deadline().as_mono_time());
    clock
        .wait_until(deadline)
        .map_err(|error| error.to_string())
}
