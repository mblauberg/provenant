//! Typed classification of unreadable processes for worktree occupancy.
//!
//! A process whose cwd or identity the kernel will not show is not, by itself,
//! proof that a lane worktree is occupied. When typed evidence proves it cannot
//! be the lane's writer, the census leaves the root free and warns naming the
//! process; a same-user process that could still be inside keeps the root
//! unknown. Nothing here parses the `process_details` diagnostic string.

use super::*;

/// A typed reason an unreadable process cannot occupy any candidate root.
fn non_occupying_reason(
    cwd_observer: &dyn CwdObserver,
    pid: i64,
    roots: &[PathBuf],
) -> Option<&'static str> {
    // Another user's process cannot hold a same-user lane checkout. The real
    // uid is the one the issue names; missing credentials never excuse.
    if let Some(uids) = cwd_observer.process_uids(pid) {
        let owners = std::iter::once(provenant_os::fs::effective_uid()).chain(
            roots
                .iter()
                .filter_map(|root| std::fs::metadata(root).ok().map(|meta| meta.uid())),
        );
        if owners.into_iter().all(|owner| uids.real != owner) {
            return Some("runs as another user");
        }
    }
    // A sealed platform binary is host-owned and never a lane writer.
    if cwd_observer.sealed_system_image(pid).is_some() {
        return Some("platform-signed system binary");
    }
    // A readable executable outside every root, with a cwd that is either
    // unreadable or also outside, cannot be a dweller in any candidate root.
    let outside = |path: &Path| {
        roots
            .iter()
            .all(|root| !super::census::occupancy_path_could_be_under(path, root))
    };
    let executable = cwd_observer.executable(pid)?;
    if !outside(&executable) {
        return None;
    }
    let cwd_outside = match cwd_observer.cwd(pid) {
        Ok(CwdObservation::Path(path)) => outside(&path),
        _ => true,
    };
    cwd_outside.then_some("executable and cwd outside the worktree")
}

/// Excuse one unreadable process from blocking the census, or return false
/// when it could still occupy a root. `placed_inside` carries positive
/// cwd/descriptor evidence, which always wins over the missing metadata.
pub(super) fn excuse_unreadable(
    cwd_observer: &dyn CwdObserver,
    pid: i64,
    roots: &[PathBuf],
    placed_inside: bool,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
) -> bool {
    if placed_inside {
        return false;
    }
    let Some(reason) = non_occupying_reason(cwd_observer, pid, roots) else {
        return false;
    };
    use provenant_records::warning::{Warning, warn};
    warn(
        Warning::new(
            provenant_records::warning::UNREADABLE_PROCESS_NON_OCCUPYING,
            format!("unreadable process cannot occupy a lane worktree ({reason})"),
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
