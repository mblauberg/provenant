//! The `PROVENANT_*` environment a consuming repository sees or may set.
//!
//! This table is the one declaration: every site that sets or reads a listed
//! variable names it through the constants below, and the outward contract
//! (`provenant-cli`'s `external_contract`) and its spec render the table. A
//! new variable is one constant and one table row. A `PROVENANT_*` name that
//! is not listed is internal by rule: test fixtures and probes (every
//! `PROVENANT_TEST_*` name among them), build-time inputs and private
//! hand-offs between Provenant processes. Consumers must not rely on one.
use crate::{RUN_STATE_DIRECTORY_ENV, STATE_DIRECTORY_ENV};
use serde::{Deserialize, Serialize};

/// How long a consumer may rely on a declared item.
///
/// `Evolving` is the default tier. An entry is `Stable` only when a consuming
/// repository relies on it today; promoting one is a one-row change.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Stability {
    /// Changed or removed only by a major release, with a release note.
    Stable,
    /// May change in any minor release, with a release note naming the change.
    #[default]
    Evolving,
    /// Provenant's own hand-off, listed so consumers know not to rely on it.
    Internal,
}

/// Who sets the variable and where it is seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Provenant sets it in the lane's environment.
    Lane,
    /// Provenant sets it in a lane, and a caller outside a lane may set it
    /// for Provenant to read.
    LaneOrCaller,
    /// Provenant sets it for a declared port command it runs.
    PortCommand,
    /// The caller sets it; Provenant only reads it.
    Caller,
}

/// One declared variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvVar {
    pub name: &'static str,
    pub channel: Channel,
    pub stability: Stability,
    pub meaning: &'static str,
    /// When it is present; a caller-set variable states what absence means.
    pub when: &'static str,
}

pub const AGENT_LABEL: &str = "PROVENANT_AGENT_LABEL";
pub const LANE_DEPTH: &str = "PROVENANT_LANE_DEPTH";
pub const PARENT_ATTEMPT_ID: &str = "PROVENANT_PARENT_ATTEMPT_ID";
pub const HOME: &str = "HOME";
pub const ROOT: &str = "PROVENANT_ROOT";
pub const GIT: &str = "PROVENANT_GIT";
pub const SANDBOX: &str = "PROVENANT_SANDBOX";
pub const PROCESS_TREE: &str = "PROVENANT_PROCESS_TREE";
pub const LANE_CARGO_LINT_TARGET: &str = "PROVENANT_LANE_CARGO_LINT_TARGET";
pub const LANE_HEAD: &str = "PROVENANT_LANE_HEAD";
pub const LANE_BASE: &str = "PROVENANT_LANE_BASE";
pub const PRIMARY_CHECKOUT: &str = "PROVENANT_PRIMARY_CHECKOUT";
pub const WORKTREE_GITDIR: &str = "PROVENANT_WORKTREE_GITDIR";
pub const LANE_PORT_BASE: &str = "PROVENANT_LANE_PORT_BASE";
pub const LANE_PORT_COUNT: &str = "PROVENANT_LANE_PORT_COUNT";
pub const CARGO_SOURCE_WRITES: &str = "PROVENANT_CARGO_SOURCE_WRITES";
pub const CARGO_SOURCE_WRITES_SOURCE: &str = "PROVENANT_CARGO_SOURCE_WRITES_SOURCE";
pub const BUILD_SLOT: &str = "PROVENANT_BUILD_SLOT";
pub const BUILD_SLOT_STATE: &str = "PROVENANT_BUILD_SLOT_STATE";
pub const BUILD_SLOTS: &str = "PROVENANT_BUILD_SLOTS";
pub const LOCAL_CARGO: &str = "PROVENANT_LOCAL_CARGO";
pub const BUILD_ADMISSION_BACKEND: &str = "PROVENANT_BUILD_ADMISSION_BACKEND";
pub const BUILD_ADMISSION_SOURCE: &str = "PROVENANT_BUILD_ADMISSION_SOURCE";
pub const BUILD_DISK_LIMITS: &str = "PROVENANT_BUILD_DISK_LIMITS";
pub const PORT_OPERATION: &str = "PROVENANT_PORT_OPERATION";
pub const PORT_SOURCE: &str = "PROVENANT_PORT_SOURCE";
pub const PORT_PHASE: &str = "PROVENANT_PORT_PHASE";
pub const ADMISSION_ATTEMPT: &str = "PROVENANT_ADMISSION_ATTEMPT";
pub const ADMISSION_WORKTREE: &str = "PROVENANT_ADMISSION_WORKTREE";
pub const ADMISSION_PROJECT: &str = "PROVENANT_ADMISSION_PROJECT";
pub const ADMISSION_OWNER_PID: &str = "PROVENANT_ADMISSION_OWNER_PID";
pub const ADMISSION_EPOCH: &str = "PROVENANT_ADMISSION_EPOCH";
pub const PORT_REQUEST_PATH: &str = "PROVENANT_PORT_REQUEST_PATH";
pub const PORT_IF_NONE_MATCH: &str = "PROVENANT_PORT_IF_NONE_MATCH";
pub const VERIFY_TIER: &str = "PROVENANT_VERIFY_TIER";
pub const VERIFY_PROJECT_ROOT: &str = "PROVENANT_VERIFY_PROJECT_ROOT";
pub const VERIFY_EFFECTIVE_CONFIG_JSON: &str = "PROVENANT_VERIFY_EFFECTIVE_CONFIG_JSON";
pub const SESSION_ID: &str = "PROVENANT_SESSION_ID";

const fn var(
    name: &'static str,
    channel: Channel,
    stability: Stability,
    meaning: &'static str,
    when: &'static str,
) -> EnvVar {
    EnvVar {
        name,
        channel,
        stability,
        meaning,
        when,
    }
}

use Channel::{Caller, Lane, LaneOrCaller, PortCommand};
use Stability::{Evolving, Internal, Stable};

const EVERY_LANE: &str = "every lane";
const BUILD_SHIM: &str = "only for the cargo child of the lane's cargo shim";
const ADMISSION: &str = "admission port commands";

/// Every declared variable. Order is presentation order only.
pub const CONSUMER_ENV: &[EnvVar] = &[
    var(
        SANDBOX,
        Lane,
        Stable,
        "`on` when the lane runs inside Provenant's native sandbox, `off` when it does not.",
        EVERY_LANE,
    ),
    var(
        LANE_DEPTH,
        Lane,
        Evolving,
        "Nesting depth: 1 for a lane the caller started, one more per nested lane. Absent or `0` means not in a lane.",
        EVERY_LANE,
    ),
    var(
        PROCESS_TREE,
        Lane,
        Stable,
        "Names the sample behind `provenant process-tree --json`. Its presence is the contract; read the sample through that command, never the file.",
        "while the supervisor samples the lane's processes",
    ),
    var(
        LANE_CARGO_LINT_TARGET,
        Lane,
        Stable,
        "A Cargo target directory beside the lane's own, for lint-only runs: point CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR at it so lints do not queue behind builds.",
        "writer lanes",
    ),
    var(
        LANE_PORT_BASE,
        Lane,
        Evolving,
        "First port of the lane's private port block.",
        "writer lanes holding an allocated port block",
    ),
    var(
        LANE_PORT_COUNT,
        Lane,
        Evolving,
        "Number of ports in the lane's private block.",
        "writer lanes holding an allocated port block",
    ),
    var(
        STATE_DIRECTORY_ENV,
        LaneOrCaller,
        Evolving,
        "State root. A caller sets it to use a root other than `~/.local/share/provenant/v2`; in a lane it names the attempt's private store.",
        "every lane; when a caller leaves it unset the default root applies",
    ),
    var(
        AGENT_LABEL,
        LaneOrCaller,
        Evolving,
        "The agent name inbox, fabric and checkpoint writes are attributed to: `lane-<attempt>` in a lane. A caller may set it to name itself.",
        "every lane; when a caller leaves it unset the harness identity applies",
    ),
    var(
        PARENT_ATTEMPT_ID,
        Lane,
        Evolving,
        "The lane's own attempt. A `provenant run` inside the lane records it as the parent.",
        EVERY_LANE,
    ),
    var(
        HOME,
        Lane,
        Evolving,
        "In a sandboxed lane, the attempt's private home, where provider credentials and caches are staged. An unsandboxed lane, the default, inherits the caller's home with its credentials and tool caches in place.",
        EVERY_LANE,
    ),
    var(
        GIT,
        LaneOrCaller,
        Evolving,
        "The Git executable Provenant runs. In a lane it is the wrapped Git the lane must use.",
        "every lane; when a caller leaves it unset Git is found on PATH",
    ),
    var(
        LANE_HEAD,
        Lane,
        Evolving,
        "The branch the writer lane delivers to.",
        "lanes whose task names a head branch",
    ),
    var(
        LANE_BASE,
        Lane,
        Evolving,
        "The branch the lane's head is based on.",
        "lanes whose task names both a head and a base",
    ),
    var(
        PRIMARY_CHECKOUT,
        Lane,
        Evolving,
        "The project's primary checkout: the directory holding the common Git directory.",
        "lanes launched in a project",
    ),
    var(
        WORKTREE_GITDIR,
        Lane,
        Evolving,
        "The linked worktree's private Git directory.",
        "writer lanes, and read-only lanes whose worktree has a private Git directory",
    ),
    var(
        SESSION_ID,
        Caller,
        Evolving,
        "Names the chair session the chair log records. When unset, settlement finds the session the chair saved.",
        "set by the caller",
    ),
    var(
        PORT_PHASE,
        PortCommand,
        Evolving,
        "The admission phase the command runs for.",
        ADMISSION,
    ),
    var(
        ADMISSION_ATTEMPT,
        PortCommand,
        Evolving,
        "The attempt being admitted.",
        ADMISSION,
    ),
    var(
        ADMISSION_WORKTREE,
        PortCommand,
        Evolving,
        "The writer's worktree.",
        ADMISSION,
    ),
    var(
        ADMISSION_PROJECT,
        PortCommand,
        Evolving,
        "The writer's project checkout.",
        ADMISSION,
    ),
    var(
        ADMISSION_OWNER_PID,
        PortCommand,
        Evolving,
        "The Provenant process that owns the admission.",
        ADMISSION,
    ),
    var(
        ADMISSION_EPOCH,
        PortCommand,
        Evolving,
        "The worktree lease epoch the admission holds.",
        ADMISSION,
    ),
    var(
        PORT_OPERATION,
        PortCommand,
        Evolving,
        "The typed forge operation requested from the repository adapter.",
        "forge port commands",
    ),
    var(
        PORT_SOURCE,
        PortCommand,
        Evolving,
        "Source of the selected build admission wrapper.",
        "build admission port commands",
    ),
    var(
        PORT_REQUEST_PATH,
        PortCommand,
        Evolving,
        "The forge request path, such as `repos/{owner}/{repo}`.",
        "CI observation and repository discovery port commands",
    ),
    var(
        PORT_IF_NONE_MATCH,
        PortCommand,
        Evolving,
        "The cached validator to send, or empty when none is sent.",
        "CI observation and repository discovery port commands",
    ),
    var(
        VERIFY_TIER,
        PortCommand,
        Evolving,
        "The verify tier being run.",
        "verify port commands",
    ),
    var(
        VERIFY_PROJECT_ROOT,
        PortCommand,
        Evolving,
        "The project checkout being verified.",
        "verify port commands",
    ),
    var(
        VERIFY_EFFECTIVE_CONFIG_JSON,
        PortCommand,
        Internal,
        "The resolved verify declaration, for Provenant's builtin verify engine.",
        "verify port commands",
    ),
    var(
        RUN_STATE_DIRECTORY_ENV,
        Lane,
        Internal,
        "Run storage for children the lane submits.",
        EVERY_LANE,
    ),
    var(
        ROOT,
        Lane,
        Internal,
        "Installed release assets.",
        EVERY_LANE,
    ),
    var(
        CARGO_SOURCE_WRITES,
        Lane,
        Internal,
        "Whether lane Cargo may write registry sources; the cargo shim explains offline failures with it.",
        "lanes with a Cargo home",
    ),
    var(
        CARGO_SOURCE_WRITES_SOURCE,
        Lane,
        Internal,
        "Where the Cargo source-write setting came from.",
        "lanes with a Cargo home",
    ),
    var(
        BUILD_ADMISSION_BACKEND,
        Lane,
        Internal,
        "Encoded build admission backend selected by the host.",
        BUILD_SHIM,
    ),
    var(
        BUILD_ADMISSION_SOURCE,
        Lane,
        Internal,
        "Source of the selected build admission backend.",
        BUILD_SHIM,
    ),
    var(
        BUILD_SLOT,
        Lane,
        Internal,
        "Marks a cargo process that holds a host build slot.",
        BUILD_SHIM,
    ),
    var(
        BUILD_SLOT_STATE,
        Lane,
        Internal,
        "The host build-slot directory.",
        BUILD_SHIM,
    ),
    var(
        BUILD_SLOTS,
        Lane,
        Internal,
        "The host build-slot count.",
        BUILD_SHIM,
    ),
    var(
        LOCAL_CARGO,
        Lane,
        Internal,
        "The real cargo the shim runs.",
        BUILD_SHIM,
    ),
    var(
        BUILD_DISK_LIMITS,
        Lane,
        Internal,
        "Encoded build disk limits.",
        BUILD_SHIM,
    ),
];

/// The declaration for `name`, when it is declared.
pub fn declared(name: &str) -> Option<&'static EnvVar> {
    CONSUMER_ENV.iter().find(|var| var.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn declared_names_are_unique_provenant_names_and_never_test_names() {
        let mut seen = BTreeSet::new();
        for var in CONSUMER_ENV {
            assert!(seen.insert(var.name), "{} is declared twice", var.name);
            assert!(
                var.name.starts_with("PROVENANT_") || var.channel == Channel::Lane,
                "{} is a foreign name Provenant does not set",
                var.name
            );
            assert!(!var.name.starts_with("PROVENANT_TEST_"), "{}", var.name);
            assert!(
                !var.meaning.is_empty() && !var.when.is_empty(),
                "{}",
                var.name
            );
            assert_eq!(declared(var.name), Some(var));
        }
        assert_eq!(declared("PROVENANT_TEST_ANY"), None);
    }
}
