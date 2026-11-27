//! `lane` — deliver a settled writer attempt, or set the owning lane's own
//! worktree sharing rule (`lane share`, #3453/#4120).
//!
//! `share` is the owning lane's in-tree decision: it is stored in that lane's
//! worktree lease, where the confinement reader `owner_lease_sharing` picks it
//! up, and wins over every other layer for that worktree only. It refuses
//! outside a lane; the host sets the layered default with
//! `lanes.worktree.sharing`.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_confinement::worktrees::LEASE_FILE;
use provenant_core::consumer_env::Stability;
use provenant_records::custody::{Sharing, WorktreeLease};
use provenant_records::{CommandError, Outcome};

/// The one setting a lane can fix for its own worktree. Keep in step with the
/// confinement reader of the same key.
const WORKTREE_SHARING_KEY: &str = provenant_records::custody::WORKTREE_SHARING_KEY;

pub static DOMAIN: Domain = Domain {
    name: "lane",
    verbs: &[
        Verb {
            path: &["lane", "deliver"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["lane", "share"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Lane(provenant_cli::Lane::Share { mode }) => return share(inv, &mode),
        Command::Lane(cmd) => crate::run_edge(inv.leaf, globals, |edge| {
            let mut reply = provenant_cli::run::lane(cmd, edge);
            if let Err(error) =
                crate::fabric::settle_delivered_attempt(edge, &mut reply, process.workers())
            {
                reply
                    .human
                    .push_str(&format!("worktree retained: {error}\n"));
                reply.result.data["worktree_settlement_error"] =
                    serde_json::json!(error.to_string());
            }
            reply
        }),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}

fn parse_sharing(mode: &str) -> Option<Sharing> {
    mode.parse::<Sharing>()
        .ok()
        .filter(|sharing| *sharing != Sharing::Inherit)
}

fn failure(code: &str, message: impl Into<String>, cause: Option<String>) -> CommandError {
    CommandError::new(
        code,
        message,
        cause.as_deref(),
        "rerun from a registered lane worktree",
    )
    .with_setting(provenant_records::command_result::SettingRef {
        key: WORKTREE_SHARING_KEY.to_owned(),
        value: cause.unwrap_or_default(),
        layer: provenant_records::Layer::Launch,
        origin: "provenant lane share".to_owned(),
    })
}

/// Persist the owner's sharing rule into the lane worktree's lease. Only the
/// `sharing` field is a decision of this verb; holder and start time are
/// preserved from an existing lease so custody is never changed here.
fn share(inv: &Call, mode: &str) -> Result<Outcome, CommandError> {
    let sharing = parse_sharing(mode).ok_or_else(|| {
        CommandError::new(
            "INVALID_INPUT",
            format!("unknown sharing mode {mode:?}; expected allow, warn or refuse"),
            None,
            "provenant lane share allow|warn|refuse",
        )
    })?;
    if !provenant_cli::fabric::in_lane() {
        return Err(failure(
            "INVALID_INPUT",
            "`lane share` sets the owning lane's worktree lease and is lane-only",
            None,
        ));
    }
    let resolved = crate::roots::resolve(
        inv.globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )
    .map_err(|error| {
        failure(
            "OPERATION_ERROR",
            "the lane's workspace could not be resolved",
            Some(error.to_string()),
        )
    })?;
    let worktree = &resolved.project;
    let project =
        provenant_cli::run::preview_project_id(&resolved.roots, worktree).ok_or_else(|| {
            failure(
                "OPERATION_ERROR",
                "the lane's project is not registered on this host",
                None,
            )
        })?;

    let state = resolved
        .roots
        .state
        .as_path()
        .join("worktrees")
        .join(project.to_string());
    let slot_name = provenant_confinement::worktrees::state_dir_name(worktree);
    let directory = provenant_os::fs::Directory::open_anchor(&state).map_err(|error| {
        failure(
            "OPERATION_ERROR",
            "the worktree registry could not be opened",
            Some(error.to_string()),
        )
    })?;
    let slot = directory
        .open_verified_directory(&slot_name)
        .map_err(|error| {
            failure(
                "OPERATION_ERROR",
                "the lane's worktree has no custody slot",
                Some(error.to_string()),
            )
        })?;

    let holder = std::env::var(provenant_core::consumer_env::PARENT_ATTEMPT_ID)
        .ok()
        .and_then(|value| value.parse().ok());
    let since = provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())
        .unwrap_or_else(|_| provenant_core::WallTime::MAX);
    // Preserve holder and start time from an existing lease in their wire
    // form; only the owning lane's sharing decision is changed here.
    let lease: WorktreeLease = slot
        .read_bounded(std::ffi::OsStr::new(LEASE_FILE), 4096)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|mut value| {
            value["sharing"] = serde_json::json!(sharing);
            serde_json::from_value(value).ok()
        })
        .unwrap_or(WorktreeLease {
            holder: holder.unwrap_or_else(|| provenant_core::AttemptId::from_bytes([0; 16])),
            since,
            sharing,
        });
    let bytes = serde_json::to_vec(&lease).map_err(|error| {
        failure(
            "OPERATION_ERROR",
            "the worktree lease could not be encoded",
            Some(error.to_string()),
        )
    })?;
    slot.atomic_write(
        std::ffi::OsStr::new(LEASE_FILE),
        ".lease-",
        &bytes,
        provenant_os::fs::PublishMode::Replace,
    )
    .and_then(|publication| publication.require_durable())
    .map_err(|error| {
        failure(
            "OPERATION_ERROR",
            "the owning lane's sharing rule could not be stored",
            Some(error.to_string()),
        )
    })?;
    Ok(Outcome::Ok)
}

#[cfg(test)]
mod lane_share_tests {
    use super::*;

    #[test]
    fn share_modes_map_to_the_durable_sharing_enum() {
        assert_eq!(parse_sharing("allow"), Some(Sharing::Allow));
        assert_eq!(parse_sharing("warn"), Some(Sharing::Warn));
        assert_eq!(parse_sharing("refuse"), Some(Sharing::Refuse));
        assert_eq!(parse_sharing("inherit"), None);
        assert_eq!(parse_sharing(""), None);
    }

    #[test]
    fn sharing_round_trips_through_a_worktree_lease() {
        let since = provenant_core::WallTime::from_micros(42).unwrap();
        let holder = provenant_core::AttemptId::from_bytes([7; 16]);
        for mode in ["allow", "warn", "refuse"] {
            let lease = WorktreeLease {
                holder,
                since,
                sharing: parse_sharing(mode).unwrap(),
            };
            let bytes = serde_json::to_vec(&lease).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["sharing"], mode);
            let read: WorktreeLease = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(read.sharing, parse_sharing(mode).unwrap());
        }
    }

    #[test]
    fn a_lane_only_verb_names_its_setting() {
        let error = failure("INVALID_INPUT", "lane-only", None);
        assert_eq!(error.setting.as_ref().unwrap().key, WORKTREE_SHARING_KEY);
    }
}
