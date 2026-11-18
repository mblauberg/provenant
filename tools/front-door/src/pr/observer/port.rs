//! CI observation selects a category in the shared forge port.
use super::super::gh::Response;
use super::ObserveError;
use provenant_config::ports::Request;
use provenant_config::{Forge, ForgePort, HeadersObservation, PortTable};
use std::{collections::BTreeMap, io, path::Path, time::Duration};

pub(crate) fn select_transport(
    state: &Path,
    project: &Path,
    request: &Request,
) -> Result<ForgePort, ObserveError> {
    let ports = PortTable::read_for(Some(project), state).map_err(|error| {
        ObserveError::Unavailable(format!("CI port configuration is unavailable: {error}"))
    })?;
    let mut forge = ForgePort::new(
        ports.ci_observation,
        "ci_observation",
        project,
        state,
        // TODO(#3371 seam): caller owner supplies the operation-scoped policy.
        std::sync::Arc::new(provenant_records::policy::Policy::default()),
    )
    .map_err(|error| ObserveError::Unavailable(format!("cannot start CI adapter: {error}")))?;
    forge.bind(request);
    Ok(forge)
}

/// Project the typed port response into the observer's HTTP view. The bound
/// request in the adapter supplies the deadline and cancellation for each page.
pub(super) fn get(forge: &mut impl Forge, path: &str, etag: Option<&str>) -> io::Result<Response> {
    let reply = Forge::get(forge, path, etag, Duration::MAX).map_err(|error| {
        use provenant_config::{PortCommandDisposition as D, PortError};
        match error {
            PortError::Executor(error) => error,
            PortError::Disabled { .. } | PortError::Disposition(D::Refused) => {
                io::Error::new(io::ErrorKind::PermissionDenied, error)
            }
            PortError::Response(_) => io::Error::new(io::ErrorKind::InvalidData, error),
            PortError::Disposition(D::TimedOut) => io::Error::new(io::ErrorKind::TimedOut, error),
            PortError::Disposition(D::Cancelled) => {
                io::Error::new(io::ErrorKind::Interrupted, error)
            }
            error => io::Error::other(error),
        }
    })?;
    let headers = match reply.headers {
        HeadersObservation::Present { values, .. } => values,
        HeadersObservation::Absent => BTreeMap::new(),
    };
    Ok(Response {
        status: reply.status.unwrap_or(200),
        headers,
        body: reply.body,
    })
}
