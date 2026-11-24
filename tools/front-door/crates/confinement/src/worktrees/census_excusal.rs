//! Typed classification of unreadable processes for worktree occupancy.
//!
//! An unreadable process is not, by itself, proof that a lane worktree is
//! occupied. Three typed policies may excuse one: the existing listed
//! service-uid policy for a path or root identity that cannot be placed, an
//! owner-uid platform daemon with an approved executable and a readable cwd
//! outside every scoped root, and a configured image entry that extends both
//! to every unreadable failure class. Every other unreadable process keeps the
//! root unknown, with the PID named in the refusal. Nothing here parses the
//! `process_details` diagnostic string.

use super::*;

/// The observation that left a process's custody unproven. The failure decides
/// which host policy, if any, may excuse it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnreadableFailure {
    /// A cwd or descriptor path could not be placed under a root by directory
    /// identity. `listed` is the already-checked service-uid policy decision.
    PathOrRootIdentityUnreadable { listed: bool },
    /// The descriptor table was refused with EPERM.
    DescriptorsDenied,
    /// The kernel refused the cwd query.
    CwdDenied,
}

fn owner_uids(roots: &[PathBuf]) -> Vec<u32> {
    let mut owners = vec![provenant_os::fs::effective_uid()];
    owners.extend(
        roots
            .iter()
            .filter_map(|root| std::fs::metadata(root).ok().map(|meta| meta.uid())),
    );
    owners
}

/// The host's listed-uid policy: an EPERM process whose signed image passes
/// `unreadable_policy_uids` and whose system uids are foreign to every root.
pub(super) fn listed_uid_excusal(
    cwd_observer: &dyn CwdObserver,
    pid: i64,
    roots: &[PathBuf],
) -> bool {
    cwd_observer
        .unreadable_policy_uids(pid, &[])
        .is_ok_and(|excusal| super::census::excusal_applies(&excusal, roots))
}

/// The host's configured exception extends the built-in defaults to any
/// unreadable failure class. The observer matches the canonical executable and
/// team identifier; a configured path that only looks similar never excuses.
fn configured_excusal(cwd_observer: &dyn CwdObserver, pid: i64) -> bool {
    cwd_observer.configured_unreadable_image(pid, &[]).is_some()
}

fn approved_daemon_executable(path: &Path) -> bool {
    ["/usr/libexec", "/System/Library", "/usr/sbin"]
        .iter()
        .any(|directory| {
            let directory = Path::new(directory);
            path.starts_with(directory)
                && path
                    .strip_prefix(directory)
                    .is_ok_and(|rest| !rest.as_os_str().is_empty())
        })
}

/// An owner-uid process with EPERM-unreadable descriptors is host-owned only
/// when all three typed signals agree: the live platform signature, an
/// approved system-agent directory, and a readable cwd outside every root.
fn owner_platform_daemon(cwd_observer: &dyn CwdObserver, pid: i64, roots: &[PathBuf]) -> bool {
    let Some(uids) = cwd_observer.process_uids(pid) else {
        return false;
    };
    if !owner_uids(roots).contains(&uids.real) {
        return false;
    }
    if cwd_observer.platform_signed_image(pid).is_none() {
        return false;
    }
    let Some(executable) = cwd_observer.executable(pid) else {
        return false;
    };
    if !approved_daemon_executable(&executable) {
        return false;
    }
    let Ok(CwdObservation::Path(cwd)) = cwd_observer.cwd(pid) else {
        return false;
    };
    roots
        .iter()
        .all(|root| !super::census::occupancy_path_could_be_under(&cwd, root))
}

/// Excuse one unreadable process from blocking the census, or return false
/// when it could still occupy a root. Positive placement always wins.
pub(super) fn excuse_unreadable(
    cwd_observer: &dyn CwdObserver,
    pid: i64,
    roots: &[PathBuf],
    failure: UnreadableFailure,
    placed_inside: bool,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
) -> bool {
    if placed_inside {
        return false;
    }
    let reason = match failure {
        UnreadableFailure::PathOrRootIdentityUnreadable { listed: true } => {
            "listed service uid cannot write a lane checkout"
        }
        UnreadableFailure::DescriptorsDenied if owner_platform_daemon(cwd_observer, pid, roots) => {
            "platform daemon with cwd outside every root"
        }
        _ if configured_excusal(cwd_observer, pid) => "configured unreadable image exception",
        _ => return false,
    };
    let path = cwd_observer
        .executable(pid)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "path unavailable".into());
    use provenant_records::warning::{Warning, warn};
    warn(
        Warning::new(
            provenant_records::warning::UNREADABLE_PROCESS_NON_OCCUPYING,
            format!("unreadable process cannot occupy a lane worktree ({reason}); path={path}"),
        )
        .subject(format!("process {pid}")),
    );
    unplaceable(Some(pid), Some(reason));
    true
}

/// Report the EPERM processes of one census as a single line with a count and
/// what they mean. Processes already warned about in this boot are omitted, so
/// repeated censuses on the same host do not repeat the warning. The per-PID
/// list is emitted only when the caller asked for `--full`.
pub(super) fn warn_unreadable_processes(boot: &str, pids: &BTreeSet<i64>) {
    let fresh: Vec<i64> = pids
        .iter()
        .copied()
        .filter(|pid| super::census::mark_unreadable_process_warning(boot, *pid))
        .collect();
    if fresh.is_empty() {
        return;
    }
    use provenant_records::warning::{UNREADABLE_PROCESS_CENSUS, Warning, warn};
    if super::census::UNREADABLE_DETAIL.load(std::sync::atomic::Ordering::Relaxed) {
        for pid in fresh {
            // Spec: docs/specs/cli.md#warnings-are-typed-values
            warn(
                Warning::new(
                    UNREADABLE_PROCESS_CENSUS,
                    "process could not be inspected (EPERM); custody is unknown for this process (no observed cwd or descriptor under candidate worktrees)",
                )
                .subject(format!("process {pid}")),
            );
        }
    } else {
        warn(
            Warning::new(
                UNREADABLE_PROCESS_CENSUS,
                format!(
                    "{} process(es) could not be inspected (EPERM); custody is unknown for those processes (no observed cwd or descriptor under candidate worktrees)",
                    fresh.len()
                ),
            )
            .subject(format!("{} unreadable processes", fresh.len())),
        );
    }
}
