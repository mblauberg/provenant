//! Provider preparation at the CLI composition edge.
use super::{PROMPT_LIMIT, Registry, route_request};
use provenant_confinement::permissions::{
    AccessMode, NetworkAccess, PathGrant, PathScope, PlanPermissions, WorktreeWriteAuthority,
};
use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
use provenant_core::{ProjectId, WorkScope};
use provenant_os::fs::{Directory, FileKind, FsError, PayloadTree, PublishMode, entropy_128};
use provenant_providers::{LaneRuntime, PrepareInput, Prompt, REGISTRY, SetupStep, Stdin};
use provenant_records::{ADAPTER_SPECS, AdapterSpec};
use provenant_records::{ResolvedRoute, RouteAlternative};
use provenant_run as run;
use run::{RouteRecord, SkipKind, SkippedRoute};
use std::io::Write;
use std::path::{Path, PathBuf};

mod command;
mod confinement;
mod continuation;
mod fixtures;
mod observation;
mod preparation;
mod project_paths;

use command::*;
use confinement::*;
use continuation::*;
use observation::*;
#[cfg(any(test, feature = "fixture"))]
use preparation::*;

pub(crate) use confinement::{find_executable, resolve_executable};
pub use confinement::{graph_inputs_directory, graph_node_inputs_directory};
pub(super) use continuation::continuation_available;
pub(super) use fixtures::start_fabric_relay;
#[cfg(feature = "fixture")]
pub use fixtures::{apply_fixture_setup, prepare_fixture, writer_fixture_plan};
pub(super) use observation::observe_codex_rollout;
use preparation::PrepareError;
pub(super) use preparation::prepare;

#[cfg(test)]
mod cargo_jobs_tests;
#[cfg(test)]
mod cargo_profile_tests;
#[cfg(test)]
mod tests;
include!("confinement_test.rs");
