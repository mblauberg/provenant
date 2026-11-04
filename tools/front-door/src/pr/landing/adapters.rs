//! Real forge and git adapters are implemented by their owning lane.
use super::{
    LandingError,
    ports::{AlarmSink, Forge, GateRunner, Repo},
};
/// The side-effect adapters used by the landing driver.
pub type Adapters = (
    Box<dyn Forge>,
    Box<dyn Repo>,
    Box<dyn GateRunner>,
    Box<dyn AlarmSink>,
);

/// Construct the real forge, repository, gate and alarm adapters for a project.
pub fn connect(project: &std::path::Path) -> Result<Adapters, LandingError> {
    let _ = project;
    Err(LandingError::Unimplemented("adapters::connect"))
}
