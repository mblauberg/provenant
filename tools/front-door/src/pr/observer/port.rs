//! Repository-declared CI observation transport implementing the common Forge port.
use super::super::gh::{Forge, Response};
use super::super::request::Request;
use super::{GhTransport, ObserveError, expired_request};
use provenant_config::{PortCommand, PortCommandDisposition, PortSource};
use provenant_os::exec::SystemExecutor;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortResponse {
    status: u16,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    /// Resource endpoints return UTF-8 bytes; a 304 response uses an empty string.
    body: String,
}

/// Select the one effective CI observation adapter for this project, bound to
/// the caller's request context until an operation binds its own.
pub(crate) fn select_transport(
    state: &Path,
    project: &Path,
    request: &Request,
) -> Result<SelectedTransport, ObserveError> {
    let ports = provenant_config::PortTable::read_for(Some(project), state).map_err(|error| {
        ObserveError::Unavailable(format!("CI port configuration is unavailable: {error}"))
    })?;
    let source = ports.ci_observation.source;
    match ports.ci_observation.backend {
        provenant_config::PortBackend::Builtin => Ok(SelectedTransport::Builtin(GhTransport {
            project: project.to_owned(),
            request: request.clone(),
        })),
        provenant_config::PortBackend::Command { command } => {
            let executor = SystemExecutor::new(1).map_err(|error| {
                ObserveError::Unavailable(format!("cannot start CI command executor: {error}"))
            })?;
            Ok(SelectedTransport::Command(CommandTransport::new(
                command,
                source,
                project.to_owned(),
                state.to_owned(),
                executor,
                request.clone(),
            )))
        }
        provenant_config::PortBackend::None => Err(ObserveError::Unavailable(
            "CI observation is disabled by the effective repository port".into(),
        )),
    }
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

    fn bind(&mut self, request: &Request) {
        match self {
            Self::Builtin(transport) => transport.bind(request),
            Self::Command(transport) => transport.bind(request),
        }
    }
}

pub(crate) struct CommandTransport {
    command: PortCommand,
    source: PortSource,
    project: PathBuf,
    state: PathBuf,
    executor: SystemExecutor,
    /// The deadline and cancellation every command request derives from.
    request: Request,
}
impl CommandTransport {
    pub(super) fn new(
        command: PortCommand,
        source: PortSource,
        project: PathBuf,
        state: PathBuf,
        executor: SystemExecutor,
        request: Request,
    ) -> Self {
        Self {
            command,
            source,
            project,
            state,
            executor,
            request,
        }
    }

    fn record(
        &self,
        path: &str,
        execution: &provenant_config::PortCommandExecution,
    ) -> io::Result<()> {
        match provenant_config::record_port_command(
            &self.state,
            "ci_observation",
            self.source,
            "request",
            path,
            execution,
            provenant_config::PortReceiptLifetime::AgeDays {
                days: provenant_config::DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS,
            },
        ) {
            Ok(provenant_config::PortCommandPublication::Durable) => Ok(()),
            Ok(provenant_config::PortCommandPublication::VisibleDurabilityUnconfirmed {
                cause,
            }) => Err(io::Error::other(format!(
                "CI command receipt is visible but durability is unconfirmed: {cause}"
            ))),
            Err(error) => Err(io::Error::other(error)),
        }
    }
}
impl Forge for CommandTransport {
    fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response> {
        let budget = expired_request(&self.request)?;
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
            budget,
            self.request.cancel(),
        );
        let response = parse_response(&execution);
        if response.is_err() && execution.disposition == PortCommandDisposition::Succeeded {
            execution.disposition = PortCommandDisposition::Unknown;
        }
        self.record(path, &execution)?;
        response
    }

    fn bind(&mut self, request: &Request) {
        self.request = request.clone();
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
