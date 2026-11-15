//! CI observation selects a category in the one shared forge adapter.
use super::super::gh::{Forge, Response};
use super::ObserveError;
use provenant_config::{ForgePort, HeadersObservation, PortTable};
use std::{collections::BTreeMap, io, path::Path, time::Duration};

pub(crate) fn select_transport(
    state: &Path,
    project: &Path,
    budget: Duration,
) -> Result<SelectedTransport, ObserveError> {
    let ports = PortTable::read_for(Some(project), state).map_err(|error| {
        ObserveError::Unavailable(format!("CI port configuration is unavailable: {error}"))
    })?;
    let forge = ForgePort::new(ports.ci_observation, "ci_observation", project, state)
        .map_err(|error| ObserveError::Unavailable(format!("cannot start CI adapter: {error}")))?;
    Ok(SelectedTransport { forge, budget })
}

pub(crate) struct SelectedTransport {
    forge: ForgePort,
    budget: Duration,
}
impl Forge for SelectedTransport {
    fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response> {
        let reply = self
            .forge
            .get(path, etag, self.budget)
            .map_err(io::Error::other)?;
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

    fn bound(&mut self, left: Duration) {
        self.budget = left;
    }
}
