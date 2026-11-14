//! Repository-declared CI observation transport implementing the common Forge port.
use super::super::gh::{Forge, Response};
use super::GhTransport;
use provenant_config::{PortCommand, PortCommandDisposition, PortSource};
use provenant_os::exec::SystemExecutor;
use serde::Deserialize;
use std::{collections::BTreeMap, ffi::OsString, io, path::PathBuf, time::Duration};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortResponse {
    status: u16,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    /// CI endpoints return JSON; a 304 response uses the empty string.
    body: String,
}

pub(crate) enum SelectedTransport {
    Builtin(GhTransport),
    Command(CommandTransport),
}
impl Forge for SelectedTransport {
    fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response> {
        match self {
            Self::Builtin(transport) => transport.get(path, etag),
            Self::Command(transport) => transport.get(path, etag),
        }
    }

    fn bound(&mut self, left: Duration) {
        match self {
            Self::Builtin(transport) => transport.bound(left),
            Self::Command(transport) => transport.bound(left),
        }
    }
}

pub(crate) struct CommandTransport {
    command: PortCommand,
    source: PortSource,
    project: PathBuf,
    state: PathBuf,
    executor: SystemExecutor,
    budget: Duration,
    cancel: provenant_core::CancelToken,
}
impl CommandTransport {
    pub(super) fn new(
        command: PortCommand,
        source: PortSource,
        project: PathBuf,
        state: PathBuf,
        executor: SystemExecutor,
        budget: Duration,
        cancel: provenant_core::CancelToken,
    ) -> Self {
        Self {
            command,
            source,
            project,
            state,
            executor,
            budget,
            cancel,
        }
    }

    fn record(
        &self,
        path: &str,
        execution: &provenant_config::PortCommandExecution,
    ) -> io::Result<()> {
        provenant_config::record_port_command(
            &self.state,
            "ci_observation",
            self.source,
            "request",
            path,
            execution,
        )
        .map_err(io::Error::other)
    }
}
impl Forge for CommandTransport {
    fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response> {
        let environment = [
            (
                OsString::from("PROVENANT_PORT_REQUEST_PATH"),
                OsString::from(path),
            ),
            (
                OsString::from("PROVENANT_PORT_IF_NONE_MATCH"),
                OsString::from(etag.unwrap_or_default()),
            ),
        ];
        let mut execution = provenant_config::execute_port_command(
            &self.executor,
            &self.command,
            &self.project,
            &[],
            &environment,
            self.budget,
            &self.cancel,
        );
        let response = parse_response(&execution);
        if response.is_err() && execution.disposition == PortCommandDisposition::Succeeded {
            execution.disposition = PortCommandDisposition::Unknown;
        }
        self.record(path, &execution)?;
        response
    }

    fn bound(&mut self, left: Duration) {
        self.budget = left;
    }
}

fn parse_response(execution: &provenant_config::PortCommandExecution) -> io::Result<Response> {
    let outcome = execution
        .outcome
        .as_ref()
        .ok_or_else(|| disposition_error(execution.disposition))?;
    if execution.disposition != PortCommandDisposition::Succeeded {
        return Err(disposition_error(execution.disposition));
    }
    if !execution.capture_complete() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "CI command port returned incomplete capture",
        ));
    }
    let reply: PortResponse = serde_json::from_slice(&outcome.stdout.bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "CI command port returned invalid response JSON",
        )
    })?;
    if !(100..=599).contains(&reply.status) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "CI command port returned an invalid HTTP status",
        ));
    }
    let mut headers = BTreeMap::new();
    for (name, value) in reply.headers {
        let name = name.to_ascii_lowercase();
        if headers.insert(name, value).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CI command port returned duplicate headers",
            ));
        }
    }
    Ok(Response {
        status: reply.status,
        headers,
        body: reply.body.into_bytes(),
    })
}

fn disposition_error(disposition: PortCommandDisposition) -> io::Error {
    let kind = match disposition {
        PortCommandDisposition::Succeeded | PortCommandDisposition::Failed => io::ErrorKind::Other,
        PortCommandDisposition::Unknown => io::ErrorKind::WouldBlock,
        PortCommandDisposition::Refused => io::ErrorKind::PermissionDenied,
        PortCommandDisposition::TimedOut => io::ErrorKind::TimedOut,
        PortCommandDisposition::Cancelled => io::ErrorKind::Interrupted,
    };
    io::Error::new(
        kind,
        format!("CI command port returned {}", disposition_name(disposition)),
    )
}

fn disposition_name(disposition: PortCommandDisposition) -> &'static str {
    match disposition {
        PortCommandDisposition::Succeeded => "succeeded",
        PortCommandDisposition::Unknown => "unknown",
        PortCommandDisposition::Refused => "refused",
        PortCommandDisposition::TimedOut => "timed_out",
        PortCommandDisposition::Cancelled => "cancelled",
        PortCommandDisposition::Failed => "failed",
    }
}
