//! Typed classification of unreadable processes for worktree occupancy.
//!
//! An unreadable process is not, by itself, proof that a lane worktree is
//! occupied. Three typed policies may excuse one: the listed service-uid
//! policy, an owner-uid platform daemon with an approved executable and a
//! readable cwd outside every scoped root, and a configured image entry that
//! extends both to every unreadable failure class. The listed policy is
//! consulted for every failure class, never for a human uid or the owner.
//!
//! Two causes kept separate: a process whose own occupancy path cannot be
//! placed (for example launchd's descriptors) may be excused by a listed uid or
//! a configured entry; a root whose own identity or metadata cannot be read
//! makes placement undecidable for that root, fails closed for every arm, and
//! keeps the root unknown. Positive placement and an undecidable root both
//! block every excusal. A listed uid the kernel will not reveal fails closed
//! too. Nothing here parses the `process_details` diagnostic string.

use super::*;

static SYSTEM_CWD_OBSERVER: SystemCwdObserver = SystemCwdObserver;

/// Wraps a host observer with a policy's configured entries. Every method
/// delegates to `inner`; the two policy methods forward `entries` (not the
/// caller's argument) so a policy cannot leak across roots. `inner` is a seam:
/// a test injects a recording observer with `with_inner` and proves the trait
/// method itself forwards non-empty entries to the matcher it calls.
pub(super) struct PolicyCwdObserver<'a> {
    entries: std::borrow::Cow<'a, [CensusIgnoreEntry]>,
    inner: &'a dyn CwdObserver,
}

impl<'a> PolicyCwdObserver<'a> {
    pub(super) fn for_roots(roots: &[PathBuf], entries: &'a [CensusIgnoreEntry]) -> Self {
        Self {
            entries: resolve_entries_for_roots(roots, entries),
            inner: &SYSTEM_CWD_OBSERVER,
        }
    }

    #[cfg(test)]
    pub(super) fn with_inner(entries: &'a [CensusIgnoreEntry], inner: &'a dyn CwdObserver) -> Self {
        Self {
            entries: std::borrow::Cow::Borrowed(entries),
            inner,
        }
    }
}

impl CwdObserver for PolicyCwdObserver<'_> {
    fn process_uids(&self, pid: i64) -> Option<provenant_os::pid::ProcessUids> {
        self.inner.process_uids(pid)
    }
    fn occupancy(&self, pid: i64) -> provenant_os::pid::OccupancyObservation {
        self.inner.occupancy(pid)
    }
    fn occupancy_while(
        &self,
        pid: i64,
        keep_going: &dyn Fn() -> bool,
    ) -> provenant_os::pid::OccupancyObservation {
        self.inner.occupancy_while(pid, keep_going)
    }
    fn executable(&self, pid: i64) -> Option<PathBuf> {
        self.inner.executable(pid)
    }
    fn sealed_system_image(&self, pid: i64) -> Option<provenant_os::pid::SealedSystemImage> {
        self.inner.sealed_system_image(pid)
    }
    fn platform_signed_image(&self, pid: i64) -> Option<provenant_os::pid::PlatformSignedImage> {
        self.inner.platform_signed_image(pid)
    }
    fn configured_unreadable_image(
        &self,
        pid: i64,
        _: &[CensusIgnoreEntry],
    ) -> Option<CensusIgnoreEntry> {
        self.inner.configured_unreadable_image(pid, &self.entries)
    }
    fn process_start_time(&self, pid: i64) -> Option<provenant_core::WallTime> {
        self.inner.process_start_time(pid)
    }
    fn kernel_thread(&self, pid: i64) -> bool {
        self.inner.kernel_thread(pid)
    }
    fn unreadable_policy_uids(
        &self,
        pid: i64,
        _: &[CensusIgnoreEntry],
    ) -> Result<
        provenant_os::pid::UnreadablePolicyExcusal,
        provenant_os::pid::UnreadablePolicyFailure,
    > {
        self.inner.unreadable_policy_uids(pid, &self.entries)
    }
    fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
        self.inner.cwd(pid)
    }
    fn process_details(&self, pid: i64) -> Option<String> {
        self.inner.process_details(pid)
    }
}

/// The observation that left a process's custody unproven. The failure decides
/// which host policy, if any, may excuse it. An undecidable root is not a
/// failure here: it is a separate blocking signal, so it can never be traded
/// for an excusal and the root keeps its Unknown state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnreadableFailure {
    /// The process's own cwd or descriptor path could not be placed under a
    /// root by directory identity (for example a path beneath a non-directory
    /// ancestor). A listed uid or a configured entry may excuse this cause.
    ProcessPathUnplaceable,
    /// The descriptor table was refused with EPERM.
    DescriptorsDenied,
    /// The kernel refused the cwd query.
    CwdDenied,
}

/// Placement of one readable path against every candidate root, decided by
/// directory identity before any excusal is consulted. Computed for the cwd and
/// each descriptor path; it mirrors `record_live_path_with_audit` rather than a
/// textual prefix test, so `/` and `/Users/user` are outside a root under
/// `/Users`, while the root itself and its descendants are inside.
#[derive(Default)]
pub(super) struct PlacementEvidence {
    inside: BTreeSet<PathBuf>,
    uncertain: BTreeSet<PathBuf>,
}

impl PlacementEvidence {
    /// Place by directory identity: canonicalise the nearest extant path, then
    /// compare the `(dev, ino)` of every ancestor with the root's. An unreadable
    /// root or ancestor is uncertain, never "outside".
    pub(super) fn from_path(path: &Path, roots: &[PathBuf]) -> Self {
        let mut evidence = Self::default();
        let Some(canonical) = super::census::canonical_cwd(path) else {
            evidence.uncertain.extend(roots.iter().cloned());
            return evidence;
        };
        if !canonical.is_absolute() {
            evidence.uncertain.extend(roots.iter().cloned());
            return evidence;
        }
        let ancestors = canonical
            .ancestors()
            .try_fold(BTreeSet::new(), |mut ids, part| {
                match std::fs::metadata(part) {
                    Ok(metadata) if metadata.is_dir() => {
                        ids.insert((metadata.dev(), metadata.ino()));
                        Some(ids)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(ids),
                    _ => None,
                }
            });
        let Some(ids) = ancestors else {
            evidence.uncertain.extend(roots.iter().cloned());
            return evidence;
        };
        for root in roots {
            match std::fs::metadata(root) {
                Ok(metadata) if metadata.is_dir() => {
                    if ids.contains(&(metadata.dev(), metadata.ino())) {
                        evidence.inside.insert(root.clone());
                    }
                }
                _ => match super::census::absent_root_contains(&canonical, root) {
                    Some(true) => {
                        evidence.inside.insert(root.clone());
                    }
                    Some(false) => {}
                    None => {
                        evidence.uncertain.insert(root.clone());
                    }
                },
            }
        }
        evidence
    }

    /// A positive placement inside any root, or an undecidable root, must win
    /// over every excusal arm, including the configured one.
    pub(super) fn blocks_excusal(&self) -> bool {
        !self.inside.is_empty() || !self.uncertain.is_empty()
    }

    /// True only when every root was decided and the path lies outside all of
    /// them. Used by the built-in platform-daemon default: an undecidable root
    /// never grants an excusal.
    pub(super) fn outside_every_root(&self) -> bool {
        !self.blocks_excusal()
    }
}

/// An excusal holds when all three credentials are system accounts foreign to
/// every candidate root's owner; a removed root is owned as its nearest
/// existing directory is. An unreadable root identity cannot be proven foreign,
/// so it fails closed rather than claiming an excusal that cannot fire.
pub(super) fn excusal_applies(
    excusal: &provenant_os::pid::UnreadablePolicyExcusal,
    roots: &[PathBuf],
) -> bool {
    excusal.uids.all_system()
        && roots.iter().all(|root| {
            super::census::root_owner(root).is_some_and(|owner| excusal.uids.all_differ_from(owner))
        })
}

fn owner_uids(roots: &[PathBuf]) -> Vec<u32> {
    let mut owners = vec![provenant_os::fs::effective_uid()];
    owners.extend(
        roots
            .iter()
            .filter_map(|root| super::census::root_owner(root)),
    );
    owners
}

/// The host's listed-uid policy: an EPERM process whose signed image passes
/// `unreadable_policy_uids` and whose system uids are foreign to every root.
pub(super) fn listed_uid_excusal(
    cwd_observer: &dyn CwdObserver,
    pid: i64,
    roots: &[PathBuf],
    entries: &[CensusIgnoreEntry],
) -> bool {
    cwd_observer
        .unreadable_policy_uids(pid, entries)
        .is_ok_and(|excusal| excusal_applies(&excusal, roots))
}

/// The host's configured exception extends the built-in defaults to any
/// unreadable failure class. The observer matches the canonical executable and
/// team identifier; a configured path that only looks similar never excuses, and
/// `started_before` requires a stable kernel start time preceding the floor by
/// at least 2 seconds.
pub(super) fn configured_entry_matches(
    cwd_observer: &dyn CwdObserver,
    pid: i64,
    entries: &[CensusIgnoreEntry],
) -> bool {
    let needs_start = entries.iter().any(|entry| entry.started_before.is_some());
    let start_before = needs_start
        .then(|| cwd_observer.process_start_time(pid))
        .flatten();
    let Some(matched) = cwd_observer.configured_unreadable_image(pid, entries) else {
        return false;
    };
    // A conditioned entry needs a known start before the floor, read twice so
    // a reused pid between the reads fails closed.
    matched.started_before.is_none()
        || (matched.started_before_allows(start_before)
            && cwd_observer.process_start_time(pid) == start_before)
}

/// Rewrite every `StartedBefore::Root` entry to `StartedBefore::At(floor)` when
/// a floor is available, or omit `Root`-conditioned entries when `floor` is `None`.
fn rewrite_root_floor(
    entries: &[CensusIgnoreEntry],
    floor: Option<provenant_core::WallTime>,
) -> Vec<CensusIgnoreEntry> {
    entries
        .iter()
        .filter_map(|entry| {
            let mut cloned = entry.clone();
            if cloned.started_before == Some(provenant_os::pid::StartedBefore::Root) {
                cloned.started_before = Some(provenant_os::pid::StartedBefore::At(floor?));
            }
            Some(cloned)
        })
        .collect()
}

pub(super) fn apply_override_floor(
    entries: Vec<CensusIgnoreEntry>,
    floor: Option<provenant_core::WallTime>,
) -> Vec<CensusIgnoreEntry> {
    match floor {
        Some(floor) => rewrite_root_floor(&entries, Some(floor)),
        None => entries,
    }
}

/// The filesystem birth time of `path` (following symlinks), or `None` when
/// the path is absent or the filesystem does not report one.
pub(super) fn birth_time(path: &std::path::Path) -> Option<provenant_core::WallTime> {
    let created = std::fs::metadata(path).ok()?.created().ok()?;
    let duration = created.duration_since(std::time::UNIX_EPOCH).ok()?;
    let micros = i64::try_from(duration.as_micros()).ok()?;
    provenant_core::WallTime::from_micros(micros).ok()
}

/// Earliest filesystem birth time across every scoped root. Every root must
/// exist and report `Metadata::created()`; any absent root or error yields
/// `None` so `StartedBefore::Root` fails closed.
fn root_creation_floor(roots: &[PathBuf]) -> Option<provenant_core::WallTime> {
    if roots.is_empty() {
        return None;
    }
    let mut earliest: Option<provenant_core::WallTime> = None;
    for root in roots {
        let floor = birth_time(root)?;
        earliest = Some(match earliest {
            Some(current) => current.min(floor),
            None => floor,
        });
    }
    earliest
}

pub(super) fn resolve_entries_for_roots<'a>(
    roots: &[PathBuf],
    entries: &'a [CensusIgnoreEntry],
) -> std::borrow::Cow<'a, [CensusIgnoreEntry]> {
    if !entries
        .iter()
        .any(|entry| entry.started_before == Some(provenant_os::pid::StartedBefore::Root))
    {
        return std::borrow::Cow::Borrowed(entries);
    }
    std::borrow::Cow::Owned(rewrite_root_floor(entries, root_creation_floor(roots)))
}

pub(super) fn unknown_worker_evidence(roots: &[PathBuf], detail: &str) -> WorkerEvidence {
    WorkerEvidence {
        states: roots
            .iter()
            .cloned()
            .map(|root| (root.clone(), Occupancy::census_unknown(root, detail)))
            .collect(),
        holders: BTreeMap::new(),
        taints: roots
            .iter()
            .cloned()
            .map(|root| {
                (
                    root,
                    TaintCause::Uncertainty(UncertaintyReason::CensusUnavailable),
                )
            })
            .collect(),
        details: roots
            .iter()
            .cloned()
            .map(|root| (root, detail.to_owned()))
            .collect(),
        held_incomplete: true,
        ..WorkerEvidence::default()
    }
}

impl super::census::CensusPolicyWorkers<'_> {
    /// Supply a proven creation floor when a scoped root (such as a removed
    /// writer worktree during abandonment) lacks a live birth time. Rewrites
    /// `StartedBefore::Root` entries to `At(floor)`.
    pub fn with_creation_floor(mut self, floor: provenant_core::WallTime) -> Self {
        self.root_floor = Some(floor);
        self.entries = rewrite_root_floor(&self.entries, Some(floor));
        self
    }
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
/// when all three typed signals agree: all three credentials belong to the
/// owner (a setuid-root image launched by the owner is not an owner daemon),
/// the live platform signature, an approved system-agent directory, and a
/// readable cwd placed outside every root by directory identity.
fn owner_platform_daemon(cwd_observer: &dyn CwdObserver, pid: i64, roots: &[PathBuf]) -> bool {
    let Some(uids) = cwd_observer.process_uids(pid) else {
        return false;
    };
    let owners = owner_uids(roots);
    if ![uids.real, uids.effective, uids.saved]
        .into_iter()
        .all(|uid| owners.contains(&uid))
    {
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
    PlacementEvidence::from_path(&cwd, roots).outside_every_root()
}

/// True when any scoped root's own identity cannot be read. Placement against
/// such a root is undecidable, so no unreadable process may be excused. An
/// absent root is decidable: placement compares paths against it lexically.
pub(super) fn any_root_identity_unreadable(roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| super::census::root_is_undecidable(root))
}

/// The placement evidence gathered for one unreadable process before any
/// excusal policy runs.
#[derive(Clone, Copy, Debug)]
pub(super) struct ExcusalEvidence {
    /// The observation that left custody unproven.
    pub failure: UnreadableFailure,
    /// The already-checked listed service-uid decision.
    pub listed: bool,
    /// A cwd or descriptor was placed inside a root.
    pub placed_inside: bool,
    /// A root's own identity could not be read, so placement is undecidable.
    pub root_undecidable: bool,
    /// The already-checked configured-image decision. It is taken once per
    /// observation so its start-time recheck brackets the one decision used.
    pub configured: bool,
}

/// Excuse one unreadable process from blocking the census, or return false
/// when it could still occupy a root. Positive placement and an undecidable
/// root both block every arm. `listed` is the already-checked service-uid
/// decision, consulted for every failure class but never for a human uid or
/// the owner.
pub(super) fn excuse_unreadable(
    cwd_observer: &dyn CwdObserver,
    pid: i64,
    roots: &[PathBuf],
    evidence: ExcusalEvidence,
    unplaceable: &mut dyn FnMut(Option<i64>, Option<&'static str>),
) -> bool {
    let ExcusalEvidence {
        failure,
        listed,
        placed_inside,
        root_undecidable,
        configured,
    } = evidence;
    if placed_inside || root_undecidable {
        return false;
    }
    let reason = if listed {
        "listed service uid cannot write a lane checkout"
    } else {
        match failure {
            UnreadableFailure::DescriptorsDenied
                if owner_platform_daemon(cwd_observer, pid, roots) =>
            {
                "platform daemon with cwd outside every root"
            }
            _ if configured => "configured unreadable image exception",
            _ => return false,
        }
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
            provenant_records::warning::detected_source(
                provenant_records::warning::UNREADABLE_PROCESS_NON_OCCUPYING,
            ),
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
                 provenant_records::warning::detected_source(UNREADABLE_PROCESS_CENSUS))
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
             provenant_records::warning::detected_source(UNREADABLE_PROCESS_CENSUS))
            .subject(format!("{} unreadable processes", fresh.len())),
        );
    }
}
