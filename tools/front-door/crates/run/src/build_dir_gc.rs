//! Bounded retention for managed private and shared Cargo target directories.
//!
//! Targets live in per-worktree lane caches and per-project shared caches. A
//! target directory is idle when a non-blocking exclusive flock succeeds on all of Cargo's
//! `.cargo-lock`, `.cargo-build-lock` and `.cargo-artifact-lock`. The cap counts
//! every managed target directory, idle and busy. Idle targets are removed least
//! recently used first until the cap and the free-space floor both hold; when
//! busy directories alone exceed the cap, every eligible idle directory goes and
//! the total stays over. A directory whose locks are held, or cannot be probed,
//! is never removed, and neither is one written within the recency grace: a
//! test or run binary executes after Cargo drops its locks. The grace does not
//! cover Cargo opening an existing lock file without writing anything; the flock
//! probe covers that case. A write dated more than a few minutes in the future
//! is clock skew, not recent activity, and does not earn the grace.
//!
//! Sizes are allocated bytes (`st_blocks * 512`) throughout. Both target roots
//! are counted, including payload outside a recognised profile, so incomplete
//! seeds and first builds cannot escape the budget. Explicit GC and launch
//! admission share the same collector.
use provenant_confinement::cache::is_cache_digest;
use provenant_os::fs::{Directory, FileKind, Metadata, PayloadTree, effective_uid};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Cargo's per-profile lock files. `.cargo-lock` marks a profile directory;
/// the other two exist only under newer Cargo and are probed when present.
#[cfg(test)]
const CARGO_LOCK: &str = ".cargo-lock";
#[cfg(test)]
const LOCKS: [&str; 3] = [CARGO_LOCK, ".cargo-build-lock", ".cargo-artifact-lock"];
const TARGETS: [&str; 2] = ["target", "target-lint"];
const GC_TOMBSTONES: [(&str, &str); 2] = [
    (".provenant-gc-target", "target"),
    (".provenant-gc-target-lint", "target-lint"),
];
const MAX_DEPTH: usize = 64;
const MAX_ENTRIES: usize = 1_000_000;
const MAX_PROTECTION_PROJECTS: usize = 10_000;
const MAX_LRU_BATCH: usize = 128;
const PROTECTION_SCAN_BUDGET: Duration = Duration::from_secs(5);
const PREVIEW_ENTRIES: usize = 4096;
const PREVIEW_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);
const SURVEY_SNAPSHOT_LIMIT: u64 = 16 * 1024 * 1024;
const SURVEY_SNAPSHOT_PREFIX: &str = "gc-cache-survey-";
const SURVEY_SNAPSHOT_VERSION: u8 = 5;
const MAX_SNAPSHOT_FILES_PER_SWEEP: usize = 128;
const TARGET_LAST_USED: &str = ".provenant-last-used";
const CACHE_ACTIVITY_MARKER: &str = ".provenant-cache-activity";
/// How far ahead of now a modification time may be and still count as a
/// recent write.
const FUTURE_SKEW_NS: i128 = 5 * 60 * 1_000_000_000;

/// Size cap on all target directories, the free-space floor to restore, and
/// the seconds after its newest write that a directory is spared.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: u64,
    pub free_floor: u64,
    pub grace_seconds: u64,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub removed: Vec<PathBuf>,
    /// Bytes reclaimed, or reclaimable on a dry run.
    pub bytes: u64,
    /// Allocated bytes under managed target roots after the sweep.
    pub cache_bytes: u64,
    pub budget_bytes: u64,
    pub budget_setting: String,
    pub budget_source: String,
    /// Profile directories kept because their locks are held or unprobeable.
    pub busy: Vec<PathBuf>,
    /// Profile directories kept because they were written within the grace.
    pub recent: Vec<PathBuf>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// Preview omitted the recursive profile census.
    pub measurement_skipped: bool,
    /// Inclusive target path to resume after the sweep budget expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<PathBuf>,
    /// Epoch-specific sidecar name for the next sweep to resume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_detail: Option<String>,
    #[serde(skip)]
    more_candidates: bool,
}

#[derive(Clone, Debug)]
struct Candidate {
    kind: CacheKind,
    cache: String,
    target: String,
    display: PathBuf,
    bytes: u64,
    newest: i128,
    target_modified_ns: i128,
    identity: (u64, u64),
}

impl serde::Serialize for Candidate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeTuple;

        let mut tuple = serializer.serialize_tuple(7)?;
        tuple.serialize_element(&self.kind)?;
        tuple.serialize_element(&self.cache)?;
        tuple.serialize_element(&self.target)?;
        tuple.serialize_element(&self.bytes)?;
        tuple.serialize_element(&self.newest)?;
        tuple.serialize_element(&self.target_modified_ns)?;
        tuple.serialize_element(&self.identity)?;
        tuple.end()
    }
}

impl<'de> serde::Deserialize<'de> for Candidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CandidateVisitor;

        impl<'de> serde::de::Visitor<'de> for CandidateVisitor {
            type Value = Candidate;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a compact Cargo target cache candidate")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                use serde::de::Error as _;

                let kind = sequence
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(0, &self))?;
                let cache = sequence
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(1, &self))?;
                let target = sequence
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(2, &self))?;
                let bytes = sequence
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(3, &self))?;
                let newest = sequence
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(4, &self))?;
                let target_modified_ns = sequence
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(5, &self))?;
                let identity = sequence
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(6, &self))?;
                Ok(Candidate {
                    kind,
                    cache,
                    target,
                    display: PathBuf::new(),
                    bytes,
                    newest,
                    target_modified_ns,
                    identity,
                })
            }
        }

        deserializer.deserialize_tuple(7, CandidateVisitor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RootIdentity {
    device: u64,
    inode: u64,
    modified_ns: i128,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct SurveySnapshot {
    version: u8,
    epoch: String,
    lane_root: Option<RootIdentity>,
    shared_root: Option<RootIdentity>,
    completed: Vec<Candidate>,
    census_bytes: u64,
    census_complete: bool,
    more_candidates: bool,
    root_cursor: Option<RootCursor>,
    prune_cursor: Option<PruneCursor>,
    measuring: Option<MeasureProgress>,
    rechecking: Option<MeasureProgress>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct PruneCursor {
    kind: CacheKind,
    cache: String,
    target: String,
    newest: i128,
    display: PathBuf,
    inclusive: bool,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
struct RootCursor {
    kind: CacheKind,
    offset: provenant_os::fs::DirectoryPosition,
    target_index: u8,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct MeasureProgress {
    kind: CacheKind,
    cache: String,
    target: String,
    identity: (u64, u64),
    stack: Vec<MeasureFrame>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct MeasureFrame {
    /// Hex-encoded native path components below the target root.
    path: Vec<String>,
    device: u64,
    inode: u64,
    modified_ns: i128,
    /// Opaque `telldir` cookie, valid while this directory remains unchanged.
    offset: provenant_os::fs::DirectoryPosition,
    bytes: u64,
    newest: i128,
}

struct MeasureBudget {
    preview: bool,
    remaining: usize,
    started: std::time::Instant,
    limited: bool,
}

impl MeasureBudget {
    fn new(preview: bool) -> Self {
        Self {
            preview,
            remaining: if preview { PREVIEW_ENTRIES } else { usize::MAX },
            started: std::time::Instant::now(),
            limited: false,
        }
    }

    fn consume(&mut self) -> bool {
        if self.preview && (self.remaining == 0 || self.started.elapsed() >= PREVIEW_BUDGET) {
            self.limited = true;
            return false;
        }
        self.remaining = self.remaining.saturating_sub(1);
        true
    }

    fn preview_limited(&mut self) -> bool {
        if self.preview && (self.remaining == 0 || self.started.elapsed() >= PREVIEW_BUDGET) {
            self.limited = true;
        }
        self.limited
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
enum CacheKind {
    Lane,
    Shared,
}

/// Active target owners observed before cache eviction. Unknown custody
/// protects all targets until a later observation can name the owners.
#[derive(Clone, Debug, Default)]
pub struct TargetProtection {
    pub worktrees: BTreeSet<PathBuf>,
    pub shared_projects: BTreeSet<String>,
    pub unobserved: bool,
}

impl TargetProtection {
    fn merge(&mut self, observed: &Self) {
        self.worktrees.extend(observed.worktrees.iter().cloned());
        self.shared_projects
            .extend(observed.shared_projects.iter().cloned());
        self.unobserved |= observed.unobserved;
    }
}

/// Snapshot live writer custody across every registered project for launch
/// eviction. Any unreadable registration, run store or request makes the whole
/// cache set conservative until a later observation succeeds.
pub fn target_protection_for_launch(
    roots: &provenant_core::Roots,
    current_worktree: Option<&Path>,
    current_common_dir: Option<&Path>,
) -> TargetProtection {
    target_protection_for_launch_while(roots, current_worktree, current_common_dir, &mut || false)
}

/// Snapshot every registered Git project's writer slots and process occupancy.
/// The caller's stop signal and a fixed wall-clock ceiling bound discovery;
/// incomplete evidence protects all cache targets.
pub fn target_protection_for_launch_while(
    roots: &provenant_core::Roots,
    current_worktree: Option<&Path>,
    current_common_dir: Option<&Path>,
    should_stop: &mut dyn FnMut() -> bool,
) -> TargetProtection {
    let mut protection = TargetProtection::default();
    if let Some(worktree) = current_worktree {
        protection.worktrees.insert(worktree.to_owned());
    }
    if let Some(common_dir) = current_common_dir {
        protection
            .shared_projects
            .insert(crate::target_seed::project_key(common_dir));
    }

    let started = Instant::now();
    let stop = std::cell::RefCell::new(should_stop);
    let keep_going = || !stop.borrow_mut()() && started.elapsed() < PROTECTION_SCAN_BUDGET;
    let mut selection_stop = || !keep_going();
    let selection = match provenant_config::registered_gc_project_selection_with_progress(
        roots,
        None,
        "0000000000000000",
        false,
        &mut selection_stop,
    ) {
        Ok(selection) => selection,
        Err(_) => {
            protection.unobserved = true;
            return protection;
        }
    };
    if selection.interrupted_at.is_some() || !selection.warnings.is_empty() {
        protection.unobserved = true;
        return protection;
    }
    let projects = selection.projects;
    if projects.len() > MAX_PROTECTION_PROJECTS {
        protection.unobserved = true;
        return protection;
    }
    let git = match provenant_os::git::git_executable() {
        Some(git) => git,
        None => {
            protection.unobserved = true;
            return protection;
        }
    };
    let utility =
        match provenant_confinement::UtilityRuntime::read_only(&git, roots.state.as_path()) {
            Ok(utility) => utility,
            Err(_) => {
                protection.unobserved = true;
                return protection;
            }
        };
    let executor = match provenant_os::exec::SystemExecutor::new(1) {
        Ok(executor) => executor,
        Err(_) => {
            protection.unobserved = true;
            return protection;
        }
    };
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let request = crate::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let mut records_observed = 0_usize;
    for binding in projects {
        if !keep_going() {
            protection.unobserved = true;
            return protection;
        }
        let checkout = binding
            .primary_checkout_hint()
            .map(Path::to_path_buf)
            .or_else(|| binding.common_dir().parent().map(Path::to_path_buf));
        let Some(checkout) = checkout else {
            protection.unobserved = true;
            return protection;
        };
        let remaining = PROTECTION_SCAN_BUDGET.saturating_sub(started.elapsed());
        let budget = match provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))
            .and_then(|budget| budget.with_timeout_cap(remaining))
        {
            Ok(budget) => budget,
            Err(_) => {
                protection.unobserved = true;
                return protection;
            }
        };
        let repo = match utility.open_repo(&executor, &checkout, budget, &cancel) {
            Ok(repo) if repo.common_git_dir() == binding.common_dir() => repo,
            _ => {
                protection.unobserved = true;
                return protection;
            }
        };
        let common = match Directory::open_anchor(binding.common_dir()) {
            Ok(common) => common,
            Err(_) => {
                protection.unobserved = true;
                return protection;
            }
        };
        if !common
            .metadata()
            .is_ok_and(|metadata| metadata.same_identity(&repo.common_identity()))
        {
            protection.unobserved = true;
            return protection;
        }
        let project_state_path = roots
            .state
            .as_path()
            .join("worktrees")
            .join(binding.project_id().to_string());
        let project_state = match Directory::open_anchor(&project_state_path) {
            Ok(directory) => Some(directory),
            Err(error) if error.is_not_found() => None,
            Err(_) => {
                protection.unobserved = true;
                return protection;
            }
        };
        let registration = project_state.as_ref().map_or(
            provenant_confinement::disk::WriterRegistration::Empty,
            provenant_confinement::disk::WriterRegistration::Directory,
        );
        let remaining = PROTECTION_SCAN_BUDGET.saturating_sub(started.elapsed());
        let budget = match provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))
            .and_then(|budget| budget.with_timeout_cap(remaining))
        {
            Ok(budget) => budget,
            Err(_) => {
                protection.unobserved = true;
                return protection;
            }
        };
        let occupancy = match provenant_confinement::disk::writer_occupancy(
            &repo,
            &utility,
            &executor,
            budget,
            &cancel,
            registration,
            &provenant_confinement::worktrees::SystemWorkerObserver,
            &keep_going,
        ) {
            Ok(occupancy) => occupancy,
            Err(_) => {
                protection.unobserved = true;
                return protection;
            }
        };
        let project_key = crate::target_seed::project_key(binding.common_dir());
        for (path, state) in occupancy {
            if !state.is_available() {
                protection.worktrees.insert(path);
                protection.shared_projects.insert(project_key.clone());
            }
        }

        let context = crate::ExecutionContext {
            state_root: roots.state.clone(),
            project_id: binding.project_id(),
            actor_id: provenant_core::ActorId::from_bytes([0; 16]),
            read_prompts: false,
        };
        let Some(store) = (match crate::RunStore::open_disk_read_only(&context, &request) {
            Ok(store) => store,
            Err(_) => {
                protection.unobserved = true;
                return protection;
            }
        }) else {
            continue;
        };
        let store = store.with_writer_registry(roots.state.as_path());
        let mut after = 0;
        loop {
            if !keep_going() {
                protection.unobserved = true;
                return protection;
            }
            let page = match store.lanes(
                &crate::LaneQuery {
                    targets: Vec::new(),
                    all: true,
                    limit: 100,
                    after,
                    newest_first: false,
                    owner: None,
                    unseen_after: None,
                },
                &request,
            ) {
                Ok(page) => page,
                Err(_) => {
                    protection.unobserved = true;
                    return protection;
                }
            };
            if page.lanes.is_empty() {
                break;
            }
            if records_observed.saturating_add(page.lanes.len()) > 10_000 {
                protection.unobserved = true;
                return protection;
            }
            records_observed = records_observed.saturating_add(page.lanes.len());
            for record in page.lanes {
                let canonical = match store.request_observed(record.attempt_id, &request) {
                    Ok(canonical) => canonical,
                    Err(_) => {
                        protection.unobserved = true;
                        return protection;
                    }
                };
                let terminal = matches!(
                    &record.state,
                    provenant_records::run::AttemptState::Terminal { .. }
                );
                let released = match store.custody_released(&record, &request) {
                    Ok(released) => released,
                    Err(_) => {
                        protection.unobserved = true;
                        return protection;
                    }
                };
                let held = !terminal || !released;
                if held {
                    if let Some(writer) = canonical.task.writer {
                        protection.worktrees.insert(writer.worktree);
                    }
                    protection
                        .shared_projects
                        .insert(crate::target_seed::project_key(binding.common_dir()));
                }
            }
            match page.next {
                Some(next) if next > after => after = next,
                _ => break,
            }
        }
    }
    protection
}

impl TargetProtection {
    fn protects(&self, kind: CacheKind, key: &OsStr, owner: Option<&Path>) -> bool {
        self.unobserved
            || match kind {
                CacheKind::Lane => owner.is_some_and(|path| self.worktrees.contains(path)),
                CacheKind::Shared => key
                    .to_str()
                    .is_some_and(|key| self.shared_projects.contains(key)),
            }
    }
}

enum Outcome {
    Removed,
    Busy,
    Recent(Option<(u64, i128)>),
    Interrupted {
        rechecking: Option<MeasureProgress>,
        detached: bool,
    },
}

/// What a walk of the lane caches found, before anything is removed.
struct Survey {
    report: Report,
    lane_root: Option<Directory>,
    shared_root: Option<Directory>,
    /// Least recently used first.
    candidates: Vec<Candidate>,
    total: u64,
    free: u64,
    more_candidates: bool,
    complete: bool,
}

impl Survey {
    fn satisfied(&self, limits: Limits) -> bool {
        self.total <= limits.max_bytes && self.free >= limits.free_floor
    }
    /// The report when nothing is removed.
    fn finish(mut self) -> Report {
        self.report.cache_bytes = self.total;
        self.report
    }
}

pub fn cache_root(home: &Path) -> PathBuf {
    home.join("Library/Caches/provenant-lane-cargo")
}

pub fn shared_cache_root(home: &Path) -> PathBuf {
    home.join("Library/Caches/provenant-shared-cargo")
}

/// Publish managed-target recency even when Cargo reuses every artifact. The
/// root marker also invalidates an in-progress census after a launch. Custom
/// paths have no cache authority and are left untouched.
pub fn mark_managed_target_use(home: &Path, target: &Path) -> io::Result<bool> {
    let roots = [cache_root(home), shared_cache_root(home)];
    let Some(managed_root) = roots
        .iter()
        .find(|root| managed_target_relative(target, root))
    else {
        return Ok(false);
    };
    let timestamp = provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())
        .map_err(io::Error::other)?;
    let bytes = format!("{}\n", timestamp.as_micros());
    let root = Directory::open_anchor(managed_root).map_err(io::Error::other)?;
    root.atomic_write(
        OsStr::new(CACHE_ACTIVITY_MARKER),
        ".provenant-cache-activity-",
        bytes.as_bytes(),
        provenant_os::fs::PublishMode::Replace,
    )
    .and_then(|publication| publication.require_durable())
    .map_err(io::Error::other)?;
    let directory = match Directory::open_anchor(target) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(io::Error::other(error)),
    };
    directory
        .atomic_write(
            OsStr::new(TARGET_LAST_USED),
            ".provenant-target-use-",
            bytes.as_bytes(),
            provenant_os::fs::PublishMode::Replace,
        )
        .and_then(|publication| publication.require_durable())
        .map_err(io::Error::other)?;
    Ok(true)
}

fn managed_target_relative(target: &Path, root: &Path) -> bool {
    let Ok(relative) = target.strip_prefix(root) else {
        return false;
    };
    let mut parts = relative.components();
    let (Some(std::path::Component::Normal(cache)), Some(std::path::Component::Normal(target))) =
        (parts.next(), parts.next())
    else {
        return false;
    };
    parts.next().is_none()
        && cache.to_str().is_some_and(is_cache_digest)
        && matches!(target.to_str(), Some("target" | "target-lint"))
}

/// Collect. The walk that measures every profile directory runs without any
/// lock, and nothing more happens when the cap and the floor already hold. Only
/// removal takes the host admission lock, so no lane starts mid-removal, and it
/// probes each candidate afresh. A dry run changes nothing and takes no lock.
/// `host` is the state root holding the admission ledger.
pub fn collect(
    host: &Path,
    home: &Path,
    limits: Limits,
    dry_run: bool,
) -> Result<Report, Box<dyn std::error::Error>> {
    collect_with_protection(
        host,
        home,
        limits,
        dry_run,
        &provenant_config::CargoCacheBudget {
            bytes: limits.max_bytes,
            setting: "unspecified".into(),
            source: provenant_records::LimitSource::Default,
        },
        &TargetProtection::default(),
    )
}

/// Collect private and shared targets under one budget. Unobserved owners are
/// retained conservatively; an over-budget result is reported for the caller
/// to warn about, never refused.
pub fn collect_with_protection(
    host: &Path,
    home: &Path,
    limits: Limits,
    dry_run: bool,
    budget: &provenant_config::CargoCacheBudget,
    protection: &TargetProtection,
) -> Result<Report, Box<dyn std::error::Error>> {
    let mut refresh = || protection.clone();
    collect_with_protection_refresh(
        host,
        home,
        limits,
        dry_run,
        budget,
        protection,
        &mut refresh,
    )
}

/// Like `collect_with_protection`, but refresh live custody after acquiring
/// lane admission and immediately before the pruning boundary.
#[expect(
    clippy::too_many_arguments,
    reason = "the cache budget and custody refresh are explicit at admission"
)]
pub fn collect_with_protection_refresh(
    host: &Path,
    home: &Path,
    limits: Limits,
    dry_run: bool,
    budget: &provenant_config::CargoCacheBudget,
    protection: &TargetProtection,
    refresh_protection: &mut dyn FnMut() -> TargetProtection,
) -> Result<Report, Box<dyn std::error::Error>> {
    let mut free = || free_bytes(home);
    let mut survey = survey(home, &mut free, dry_run);
    survey.report.budget_bytes = budget.bytes;
    survey.report.budget_setting = budget.setting.clone();
    survey.report.budget_source = budget.source.as_str().to_owned();
    if protection.unobserved {
        survey.report.warnings.push(
            "live Cargo target custody was not fully observed; preserving cache targets".into(),
        );
    }
    if !survey.complete {
        return Ok(survey.finish());
    }
    if survey.satisfied(limits) {
        return Ok(survey.finish());
    }
    if dry_run {
        if survey.report.measurement_skipped {
            return Ok(survey.finish());
        }
        return Ok(prune(
            survey,
            limits,
            true,
            wall_ns()?,
            &mut free,
            protection,
        ));
    }
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let request = crate::Request {
        clock: &clock,
        cancel: &cancel,
    };
    crate::ledger::while_admission_held(
        host,
        &request,
        || -> Result<Report, Box<dyn std::error::Error>> {
            // The walk may have been long, and a lane may have settled meanwhile.
            survey.free = free().unwrap_or(survey.free);
            let mut current_protection = protection.clone();
            current_protection.merge(&refresh_protection());
            Ok(prune(
                survey,
                limits,
                false,
                wall_ns()?,
                &mut free,
                &current_protection,
            ))
        },
    )?
}

/// Collect with a caller-owned deadline and resumable census. A partial census
/// never authorises deletion; completed measurements live in an epoch-specific
/// sidecar and are revalidated before pruning.
#[expect(
    clippy::too_many_arguments,
    reason = "GC admission, cache protection and continuation state stay explicit"
)]
pub fn collect_with_progress(
    host: &Path,
    home: &Path,
    limits: Limits,
    dry_run: bool,
    budget: &provenant_config::CargoCacheBudget,
    protection: &TargetProtection,
    resume_detail: Option<&str>,
    epoch: &str,
    should_stop: &mut dyn FnMut() -> bool,
    refresh_protection: &mut dyn FnMut() -> TargetProtection,
) -> Result<Report, Box<dyn std::error::Error>> {
    let mut free = || free_bytes(home);
    let mut report = Report::default();
    if !valid_epoch(epoch) {
        report.errors.push("invalid GC survey epoch".into());
        return Ok(report);
    }
    let mut snapshot = load_survey_snapshot(host, home, resume_detail, epoch, &mut report);
    let mut survey = survey_with_progress(home, &mut free, dry_run, &mut snapshot, should_stop);
    survey.report.budget_bytes = budget.bytes;
    survey.report.budget_setting = budget.setting.clone();
    survey.report.budget_source = budget.source.as_str().to_owned();
    if protection.unobserved {
        survey.report.warnings.push(
            "live Cargo target custody was not fully observed; preserving cache targets".into(),
        );
    }

    if !survey.complete {
        survey.report.measurement_skipped = true;
        let interrupted = survey.report.interrupted_at.clone();
        let mut partial = survey.finish();
        partial.interrupted_at = interrupted;
        if !dry_run {
            save_survey_snapshot(host, &mut snapshot, epoch, &mut partial);
        }
        return Ok(partial);
    }

    snapshot.completed = survey.candidates.clone();
    snapshot.measuring = None;
    if survey.satisfied(limits) {
        return Ok(survey.finish());
    }

    if dry_run {
        if should_stop() {
            let mut report = survey.finish();
            report.measurement_skipped = true;
            report.interrupted_at = snapshot
                .completed
                .first()
                .map(|candidate| candidate.display.clone());
            return Ok(report);
        }
        let mut current_protection = protection.clone();
        current_protection.merge(&refresh_protection());
        let report = prune_with_progress(
            survey,
            limits,
            true,
            wall_ns()?,
            &mut free,
            &current_protection,
            should_stop,
            None,
        );
        return Ok(report);
    }

    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let request = crate::Request {
        clock: &clock,
        cancel: &cancel,
    };
    crate::ledger::while_admission_held(
        host,
        &request,
        || -> Result<Report, Box<dyn std::error::Error>> {
            if should_stop() {
                let next_candidate = snapshot
                    .completed
                    .first()
                    .map(|candidate| candidate.display.clone());
                let mut report = survey.finish();
                report.measurement_skipped = true;
                report.interrupted_at = next_candidate;
                save_survey_snapshot(host, &mut snapshot, epoch, &mut report);
                return Ok(report);
            }
            survey.free = free().unwrap_or(survey.free);
            let mut current_protection = protection.clone();
            current_protection.merge(&refresh_protection());
            let mut aggregate = Report::default();
            aggregate.budget_bytes = budget.bytes;
            aggregate.budget_setting = budget.setting.clone();
            aggregate.budget_source = budget.source.as_str().to_owned();
            if protection.unobserved || current_protection.unobserved {
                aggregate.warnings.push(
                    "live Cargo target custody was not fully observed; preserving cache targets"
                        .into(),
                );
            }
            let mut current = survey;
            loop {
                let had_more = current.more_candidates;
                let batch = prune_with_progress(
                    current,
                    limits,
                    false,
                    wall_ns()?,
                    &mut free,
                    &current_protection,
                    should_stop,
                    Some(&mut snapshot),
                );
                merge_report(&mut aggregate, batch);
                if aggregate.interrupted_at.is_some() {
                    save_survey_snapshot(host, &mut snapshot, epoch, &mut aggregate);
                    return Ok(aggregate);
                }
                let free_now = free().unwrap_or(0);
                if aggregate.cache_bytes <= limits.max_bytes && free_now >= limits.free_floor {
                    snapshot.prune_cursor = None;
                    snapshot.census_complete = false;
                    snapshot.root_cursor = None;
                    return Ok(aggregate);
                }
                if !had_more {
                    snapshot.prune_cursor = None;
                    snapshot.census_complete = false;
                    snapshot.root_cursor = None;
                    return Ok(aggregate);
                }
                if should_stop() {
                    aggregate.measurement_skipped = true;
                    aggregate.interrupted_at = Some(cache_root(home));
                    save_survey_snapshot(host, &mut snapshot, epoch, &mut aggregate);
                    return Ok(aggregate);
                }
                snapshot.completed.clear();
                snapshot.census_bytes = 0;
                snapshot.census_complete = false;
                snapshot.more_candidates = false;
                snapshot.root_cursor = None;
                snapshot.measuring = None;
                snapshot.rechecking = None;
                let mut next =
                    survey_with_progress(home, &mut free, false, &mut snapshot, should_stop);
                let next_report = std::mem::take(&mut next.report);
                aggregate.warnings.extend(next_report.warnings);
                aggregate.errors.extend(next_report.errors);
                if !next.complete {
                    aggregate.cache_bytes = next.total;
                    aggregate.measurement_skipped = true;
                    aggregate.interrupted_at = next_report
                        .interrupted_at
                        .or_else(|| Some(cache_root(home)));
                    save_survey_snapshot(host, &mut snapshot, epoch, &mut aggregate);
                    return Ok(aggregate);
                }
                current = next;
            }
        },
    )?
}

fn merge_report(into: &mut Report, mut next: Report) {
    into.bytes = into.bytes.saturating_add(next.bytes);
    into.cache_bytes = next.cache_bytes;
    into.removed.append(&mut next.removed);
    into.busy.append(&mut next.busy);
    into.recent.append(&mut next.recent);
    into.errors.append(&mut next.errors);
    into.warnings.append(&mut next.warnings);
    into.measurement_skipped |= next.measurement_skipped;
    into.more_candidates = next.more_candidates;
    into.interrupted_at = next.interrupted_at;
    if next.cursor_detail.is_some() {
        into.cursor_detail = next.cursor_detail;
    }
}

fn valid_epoch(epoch: &str) -> bool {
    epoch.len() == 16
        && epoch
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn snapshot_name(epoch: &str) -> Option<String> {
    valid_epoch(epoch).then(|| format!("{SURVEY_SNAPSHOT_PREFIX}{epoch}.json"))
}

fn snapshot_epoch(name: &str) -> Option<&str> {
    let epoch = name
        .strip_prefix(SURVEY_SNAPSHOT_PREFIX)?
        .strip_suffix(".json")?;
    valid_epoch(epoch).then_some(epoch)
}

fn snapshot_directory(host: &Path, create: bool) -> Result<Directory, Box<dyn std::error::Error>> {
    Ok(provenant_config::private_directory(
        &host.join("tick/gc-cache-surveys"),
        create,
    )?)
}

fn load_survey_snapshot(
    host: &Path,
    home: &Path,
    detail: Option<&str>,
    epoch: &str,
    report: &mut Report,
) -> SurveySnapshot {
    let Some(name) = detail.filter(|name| snapshot_epoch(name).is_some()) else {
        return SurveySnapshot {
            version: SURVEY_SNAPSHOT_VERSION,
            epoch: epoch.into(),
            ..SurveySnapshot::default()
        };
    };
    let directory = match snapshot_directory(host, false) {
        Ok(directory) => directory,
        Err(error) => {
            report
                .warnings
                .push(format!("GC survey sidecar unavailable: {error}"));
            return SurveySnapshot {
                version: SURVEY_SNAPSHOT_VERSION,
                epoch: epoch.into(),
                ..SurveySnapshot::default()
            };
        }
    };
    let parsed = directory
        .read_stable(OsStr::new(name), SURVEY_SNAPSHOT_LIMIT)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            serde_json::from_slice::<SurveySnapshot>(&bytes).map_err(|error| error.to_string())
        });
    match parsed {
        Ok(mut snapshot)
            if snapshot.version == SURVEY_SNAPSHOT_VERSION
                && snapshot.epoch == snapshot_epoch(name).unwrap_or_default() =>
        {
            for candidate in &mut snapshot.completed {
                candidate.display = candidate_display(home, candidate);
            }
            snapshot
        }
        Ok(_) => {
            report
                .warnings
                .push("GC survey sidecar identity changed; restarting census".into());
            SurveySnapshot {
                version: SURVEY_SNAPSHOT_VERSION,
                epoch: epoch.into(),
                ..SurveySnapshot::default()
            }
        }
        Err(error) => {
            report
                .warnings
                .push(format!("GC survey sidecar ignored: {error}"));
            SurveySnapshot {
                version: SURVEY_SNAPSHOT_VERSION,
                epoch: epoch.into(),
                ..SurveySnapshot::default()
            }
        }
    }
}

fn candidate_display(home: &Path, candidate: &Candidate) -> PathBuf {
    let root = match candidate.kind {
        CacheKind::Lane => cache_root(home),
        CacheKind::Shared => shared_cache_root(home),
    };
    let logical_name = GC_TOMBSTONES
        .iter()
        .find_map(|(tombstone, logical)| (candidate.target == *tombstone).then_some(*logical))
        .unwrap_or(&candidate.target);
    root.join(&candidate.cache).join(logical_name)
}

fn save_survey_snapshot(
    host: &Path,
    snapshot: &mut SurveySnapshot,
    epoch: &str,
    report: &mut Report,
) {
    let Some(name) = snapshot_name(epoch) else {
        report.errors.push("invalid GC survey epoch".into());
        return;
    };
    snapshot.version = SURVEY_SNAPSHOT_VERSION;
    snapshot.epoch = epoch.into();
    let mut bounded = BoundedSidecar::new(SURVEY_SNAPSHOT_LIMIT as usize);
    let bytes = match serde_json::to_writer(&mut bounded, snapshot) {
        Ok(()) => bounded.bytes,
        Err(_) if bounded.exceeded => {
            report
                .errors
                .push("GC survey sidecar exceeded its size limit".into());
            return;
        }
        Err(error) => {
            report
                .errors
                .push(format!("GC survey sidecar encode: {error}"));
            return;
        }
    };
    let directory = match snapshot_directory(host, true) {
        Ok(directory) => directory,
        Err(error) => {
            report
                .warnings
                .push(format!("GC survey sidecar unavailable: {error}"));
            return;
        }
    };
    let write = directory
        .atomic_write(
            OsStr::new(&name),
            ".gc-cache-survey-",
            &bytes,
            provenant_os::fs::PublishMode::Replace,
        )
        .and_then(|publication| publication.require_durable());
    if let Err(error) = write {
        report
            .warnings
            .push(format!("GC survey checkpoint failed: {error}"));
        return;
    }
    report.cursor_detail = Some(name.clone());
}

struct BoundedSidecar {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedSidecar {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(4096)),
            limit,
            exceeded: false,
        }
    }
}

impl io::Write for BoundedSidecar {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("GC survey sidecar size limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Remove old epoch snapshots after the caller has acquired the GC lease and
/// read the active cursor. The current cursor sidecar remains available for
/// the collector to validate and resume.
pub fn prune_stale_survey_snapshots(host: &Path, active_detail: Option<&str>) -> Vec<String> {
    let Ok(directory) = snapshot_directory(host, false) else {
        return Vec::new();
    };
    let keep = active_detail
        .filter(|name| snapshot_epoch(name).is_some())
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut report = Report::default();
    let mut never_stop = || false;
    prune_survey_snapshots(&directory, &keep, &mut never_stop, &mut report);
    report.warnings
}

fn prune_survey_snapshots(
    directory: &Directory,
    keep: &BTreeSet<&str>,
    should_stop: &mut dyn FnMut() -> bool,
    report: &mut Report,
) {
    let mut scanned = 0;
    let entries = match directory.entries() {
        Ok(entries) => entries,
        Err(error) => {
            report
                .warnings
                .push(format!("GC survey sidecar scan: {error}"));
            return;
        }
    };
    for entry in entries {
        if should_stop() || scanned >= MAX_SNAPSHOT_FILES_PER_SWEEP {
            report
                .warnings
                .push("GC survey sidecar cleanup deferred to the next sweep".into());
            break;
        }
        scanned += 1;
        let name = match entry {
            Ok(name) => name,
            Err(error) => {
                report
                    .warnings
                    .push(format!("GC survey sidecar entry: {error}"));
                continue;
            }
        };
        let Some(name_text) = name.to_str() else {
            continue;
        };
        if snapshot_epoch(name_text).is_none() || keep.contains(name_text) {
            continue;
        }
        match directory.metadata_at(&name, false) {
            Ok(metadata)
                if metadata.kind == FileKind::Regular && metadata.uid == effective_uid() =>
            {
                if let Err(error) = directory.unlink(&name) {
                    report
                        .warnings
                        .push(format!("GC survey sidecar cleanup: {error}"));
                }
            }
            Ok(_) => report
                .warnings
                .push(format!("GC survey sidecar retained: {}", name_text)),
            Err(error) => report
                .warnings
                .push(format!("GC survey sidecar stat: {error}")),
        }
    }
}

fn free_bytes(home: &Path) -> io::Result<u64> {
    provenant_confinement::disk::available_bytes_at_existing(&cache_root(home))
}

/// Wall time as nanoseconds since the Unix epoch, the unit of file times.
fn wall_ns() -> Result<i128, Box<dyn std::error::Error>> {
    let now = provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())?;
    Ok(i128::from(now.as_micros()) * 1_000)
}

/// The collection itself, with free space and the time injected. The caller
/// holds whatever excludes new admissions.
pub fn sweep(
    home: &Path,
    limits: Limits,
    dry_run: bool,
    now_ns: i128,
    free_space: &mut dyn FnMut() -> io::Result<u64>,
) -> Report {
    let survey = survey(home, free_space, dry_run);
    if !survey.complete {
        return survey.finish();
    }
    prune(
        survey,
        limits,
        dry_run,
        now_ns,
        free_space,
        &TargetProtection::default(),
    )
}

/// Measure every managed target directory and sample free space. No lock is needed:
/// removal probes each candidate again.
fn survey(home: &Path, free_space: &mut dyn FnMut() -> io::Result<u64>, preview: bool) -> Survey {
    let mut report = Report::default();
    let lane_root = match Directory::open_anchor(&cache_root(home)) {
        Ok(root) => Some(root),
        Err(error) => {
            if !error.is_not_found() {
                report.errors.push(error.to_string());
            }
            None
        }
    };
    let shared_root = match Directory::open_anchor(&shared_cache_root(home)) {
        Ok(root) => Some(root),
        Err(error) if error.is_not_found() => None,
        Err(error) => {
            report.errors.push(error.to_string());
            None
        }
    };
    let mut budget = MeasureBudget::new(preview);
    let mut candidates = lane_root.as_ref().map_or_else(Vec::new, |root| {
        discover(
            root,
            &cache_root(home),
            CacheKind::Lane,
            &mut report,
            &mut budget,
        )
    });
    if let Some(root) = &shared_root {
        candidates.extend(discover(
            root,
            &shared_cache_root(home),
            CacheKind::Shared,
            &mut report,
            &mut budget,
        ));
    }
    report.measurement_skipped = budget.limited;
    let total = candidates
        .iter()
        .fold(0_u64, |sum, item| sum.saturating_add(item.bytes));
    let free = free_space().unwrap_or_else(|error| {
        report.errors.push(format!("free space: {error}"));
        u64::MAX
    });
    // Least recently used first; the path breaks ties deterministically.
    candidates.sort_by(compare_candidates);
    let complete = !budget.limited && report.errors.is_empty();
    Survey {
        report,
        lane_root,
        shared_root,
        candidates,
        total,
        free,
        more_candidates: false,
        complete,
    }
}

/// Continue an epoch-fenced target census, checking the caller's deadline
/// between every directory entry and preserving the native stream cookies.
fn survey_with_progress(
    home: &Path,
    free_space: &mut dyn FnMut() -> io::Result<u64>,
    preview: bool,
    snapshot: &mut SurveySnapshot,
    should_stop: &mut dyn FnMut() -> bool,
) -> Survey {
    let mut report = Report::default();
    let lane_root = open_cache_root(&cache_root(home), &mut report);
    let shared_root = open_cache_root(&shared_cache_root(home), &mut report);
    let lane_identity = root_identity(lane_root.as_ref(), &mut report);
    let shared_identity = root_identity(shared_root.as_ref(), &mut report);
    if snapshot.version != SURVEY_SNAPSHOT_VERSION
        || snapshot.lane_root != lane_identity
        || snapshot.shared_root != shared_identity
    {
        if !snapshot.completed.is_empty() || snapshot.measuring.is_some() {
            report
                .warnings
                .push("GC survey roots changed; restarting census".into());
        }
        snapshot.census_bytes = 0;
        snapshot.census_complete = false;
        snapshot.more_candidates = false;
        snapshot.completed.clear();
        snapshot.census_bytes = 0;
        snapshot.census_complete = false;
        snapshot.more_candidates = false;
        snapshot.root_cursor = None;
        snapshot.prune_cursor = None;
        snapshot.measuring = None;
        snapshot.rechecking = None;
    }
    if !snapshot.census_complete && snapshot.root_cursor.is_none() {
        snapshot.completed.clear();
        snapshot.census_bytes = 0;
        snapshot.more_candidates = false;
        snapshot.measuring = None;
    }
    snapshot.version = SURVEY_SNAPSHOT_VERSION;
    snapshot.lane_root = lane_identity;
    snapshot.shared_root = shared_identity;

    if snapshot.census_complete {
        let candidates = snapshot.completed.clone();
        let free = free_space().unwrap_or_else(|error| {
            report.errors.push(format!("free space: {error}"));
            u64::MAX
        });
        report.more_candidates = snapshot.more_candidates;
        return Survey {
            report,
            lane_root,
            shared_root,
            candidates,
            total: snapshot.census_bytes,
            free,
            more_candidates: snapshot.more_candidates,
            complete: true,
        };
    }

    let mut measurement_budget = MeasureBudget::new(preview);
    let mut stop = || should_stop() || !measurement_budget.consume();
    let mut candidates = snapshot.completed.clone();
    let mut interrupted = None;
    for (root, cache_path, kind) in [
        (lane_root.as_ref(), cache_root(home), CacheKind::Lane),
        (
            shared_root.as_ref(),
            shared_cache_root(home),
            CacheKind::Shared,
        ),
    ] {
        let Some(root) = root else { continue };
        if !discover_with_progress(
            root,
            &cache_path,
            kind,
            &mut report,
            &mut candidates,
            snapshot,
            &mut stop,
            &mut interrupted,
        ) {
            break;
        }
    }
    drop(stop);
    let complete = interrupted.is_none() && !measurement_budget.limited && report.errors.is_empty();
    report.measurement_skipped = !complete;
    if let Some(path) = interrupted {
        report.interrupted_at = Some(path);
    } else if !complete && report.interrupted_at.is_none() {
        report.interrupted_at = Some(cache_root(home));
    }
    let total = snapshot.census_bytes;
    let free = free_space().unwrap_or_else(|error| {
        report.errors.push(format!("free space: {error}"));
        u64::MAX
    });
    candidates.sort_by(compare_candidates);
    if complete {
        if snapshot.rechecking.as_ref().is_some_and(|state| {
            !candidates
                .iter()
                .any(|candidate| state_matches(state, candidate))
        }) {
            snapshot.rechecking = None;
        }
        snapshot.completed = candidates.clone();
        snapshot.measuring = None;
        snapshot.census_complete = true;
        snapshot.root_cursor = None;
    }
    report.more_candidates = snapshot.more_candidates;
    Survey {
        report,
        lane_root,
        shared_root,
        candidates,
        total,
        free,
        more_candidates: snapshot.more_candidates,
        complete,
    }
}

fn open_cache_root(path: &Path, report: &mut Report) -> Option<Directory> {
    match Directory::open_anchor(path) {
        Ok(root) => Some(root),
        Err(error) if error.is_not_found() => None,
        Err(error) => {
            report.errors.push(format!("{}: {error}", path.display()));
            None
        }
    }
}

fn root_identity(root: Option<&Directory>, report: &mut Report) -> Option<RootIdentity> {
    root.and_then(|root| match root.metadata() {
        Ok(metadata) => Some(RootIdentity {
            device: metadata.device,
            inode: metadata.inode,
            modified_ns: metadata.modified_ns,
        }),
        Err(error) => {
            report.errors.push(format!("cache root identity: {error}"));
            None
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn discover_with_progress(
    root: &Directory,
    cache_root: &Path,
    kind: CacheKind,
    report: &mut Report,
    candidates: &mut Vec<Candidate>,
    snapshot: &mut SurveySnapshot,
    should_stop: &mut dyn FnMut() -> bool,
    interrupted: &mut Option<PathBuf>,
) -> bool {
    let saved_cursor = snapshot.root_cursor;
    if saved_cursor
        .is_some_and(|cursor| cursor.kind == CacheKind::Shared && kind == CacheKind::Lane)
    {
        return true;
    }
    let mut entries = match root.entries() {
        Ok(entries) => entries,
        Err(error) => {
            report
                .errors
                .push(format!("{}: {error}", cache_root.display()));
            return true;
        }
    };
    let start_offset = match saved_cursor {
        Some(cursor) if cursor.kind == kind => cursor.offset,
        _ => Default::default(),
    };
    if let Err(error) = entries.seek(start_offset) {
        report
            .errors
            .push(format!("{}: {error}", cache_root.display()));
        snapshot.completed.clear();
        snapshot.census_bytes = 0;
        snapshot.census_complete = false;
        snapshot.more_candidates = false;
        snapshot.root_cursor = Some(RootCursor {
            kind,
            offset: Default::default(),
            target_index: 0,
        });
        snapshot.measuring = None;
        return false;
    }
    loop {
        let entry_offset = match entries.position() {
            Ok(offset) => offset,
            Err(error) => {
                report
                    .errors
                    .push(format!("{}: {error}", cache_root.display()));
                return false;
            }
        };
        let Some(entry) = entries.next() else {
            snapshot.root_cursor = match kind {
                CacheKind::Lane => Some(RootCursor {
                    kind: CacheKind::Shared,
                    offset: Default::default(),
                    target_index: 0,
                }),
                CacheKind::Shared => None,
            };
            snapshot.completed = candidates.clone();
            snapshot.measuring = None;
            return true;
        };
        let name = match entry {
            Ok(name) => name,
            Err(error) => {
                report
                    .errors
                    .push(format!("{}: {error}", cache_root.display()));
                continue;
            }
        };
        if should_stop() {
            snapshot.completed = candidates.clone();
            snapshot.root_cursor = Some(RootCursor {
                kind,
                offset: entry_offset,
                target_index: 0,
            });
            *interrupted = Some(
                name.to_str()
                    .filter(|name| is_cache_digest(name))
                    .map_or_else(
                        || cache_root.to_owned(),
                        |digest| cache_root.join(digest).join("target"),
                    ),
            );
            return false;
        }
        let Some(digest) = name.to_str().filter(|name| is_cache_digest(name)) else {
            continue;
        };
        let metadata = match root.metadata_at(&name, false) {
            Ok(metadata)
                if metadata.kind == FileKind::Directory
                    && metadata.uid == effective_uid()
                    && (kind != CacheKind::Lane || metadata.mode & 0o7777 == 0o700)
                    && (kind != CacheKind::Shared || metadata.mode & 0o022 == 0) =>
            {
                metadata
            }
            _ => continue,
        };
        let Ok(cache) = PayloadTree::claim(root, &name, &metadata) else {
            continue;
        };
        let target_start = saved_cursor
            .filter(|cursor| cursor.kind == kind && cursor.offset == entry_offset)
            .map_or(0, |cursor| usize::from(cursor.target_index));
        for (target_index, (target_name, logical_name)) in TARGETS
            .into_iter()
            .map(|name| (name, name))
            .chain(GC_TOMBSTONES)
            .enumerate()
            .skip(target_start)
        {
            if should_stop() {
                snapshot.completed = candidates.clone();
                snapshot.root_cursor = Some(RootCursor {
                    kind,
                    offset: entry_offset,
                    target_index: target_index as u8,
                });
                *interrupted = Some(cache_root.join(digest).join(logical_name));
                return false;
            }
            let target = OsStr::new(target_name);
            let target_metadata = match cache.metadata_at(target) {
                Ok(metadata) if metadata.kind == FileKind::Directory => metadata,
                Ok(_) => continue,
                Err(error) if error.is_not_found() => continue,
                Err(error) => {
                    report.errors.push(format!(
                        "{}: {error}",
                        cache_root.join(digest).join(logical_name).display()
                    ));
                    continue;
                }
            };
            let candidate = Candidate {
                kind,
                cache: digest.to_owned(),
                target: target_name.to_owned(),
                display: cache_root.join(digest).join(logical_name),
                bytes: 0,
                newest: if target_name == logical_name {
                    target_metadata.modified_ns
                } else {
                    i128::MIN
                },
                target_modified_ns: target_metadata.modified_ns,
                identity: (target_metadata.device, target_metadata.inode),
            };
            let resumed = snapshot
                .measuring
                .take()
                .filter(|state| state_matches(state, &candidate));
            match measure_resumable(&cache, &candidate, &target_metadata, resumed, should_stop) {
                Ok(MeasureOutcome::Complete { bytes, newest }) => {
                    let measured = Candidate {
                        bytes,
                        newest,
                        ..candidate
                    };
                    snapshot.census_bytes = snapshot.census_bytes.saturating_add(bytes);
                    retain_lru_candidate(snapshot, candidates, measured);
                }
                Ok(MeasureOutcome::Interrupted(state)) => {
                    snapshot.measuring = Some(state);
                    snapshot.completed = candidates.clone();
                    snapshot.root_cursor = Some(RootCursor {
                        kind,
                        offset: entry_offset,
                        target_index: target_index as u8,
                    });
                    *interrupted = Some(candidate.display);
                    return false;
                }
                Ok(MeasureOutcome::Changed) => {
                    snapshot.measuring = None;
                    // A concurrent mutation invalidates the traversal cookie.
                    // Defer this target to the next sweep instead of restarting
                    // the same large tree until the budget expires.
                    snapshot.completed = candidates.clone();
                    snapshot.root_cursor = Some(RootCursor {
                        kind,
                        offset: entry_offset,
                        target_index: target_index as u8,
                    });
                    *interrupted = Some(candidate.display);
                    return false;
                }
                Err(error) => {
                    report
                        .errors
                        .push(format!("{}: {error}", candidate.display.display()));
                    snapshot.measuring = None;
                    snapshot.completed = candidates.clone();
                    snapshot.root_cursor = Some(RootCursor {
                        kind,
                        offset: entry_offset,
                        target_index: target_index as u8,
                    });
                    *interrupted = Some(candidate.display);
                    return false;
                }
            }
        }
        let offset = match entries.position() {
            Ok(offset) => offset,
            Err(error) => {
                report
                    .errors
                    .push(format!("{}: {error}", cache_root.display()));
                return false;
            }
        };
        snapshot.root_cursor = Some(RootCursor {
            kind,
            offset,
            target_index: 0,
        });
    }
}

fn same_candidate(saved: &Candidate, current: &Candidate) -> bool {
    saved.kind == current.kind
        && saved.cache == current.cache
        && saved.target == current.target
        && saved.target_modified_ns == current.target_modified_ns
        && saved.identity == current.identity
}

fn retain_lru_candidate(
    snapshot: &mut SurveySnapshot,
    candidates: &mut Vec<Candidate>,
    candidate: Candidate,
) {
    if let Some(cursor) = snapshot.prune_cursor.as_ref() {
        let ordering = compare_candidate_cursor(&candidate, cursor);
        if ordering.is_lt() || (ordering.is_eq() && !cursor.inclusive) {
            return;
        }
    }
    candidates.push(candidate);
    candidates.sort_by(compare_candidates);
    if candidates.len() > MAX_LRU_BATCH {
        candidates.pop();
        snapshot.more_candidates = true;
    }
}

fn compare_candidates(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    (a.newest, &a.display, a.kind, &a.cache, &a.target)
        .cmp(&(b.newest, &b.display, b.kind, &b.cache, &b.target))
}

fn compare_candidate_cursor(candidate: &Candidate, cursor: &PruneCursor) -> std::cmp::Ordering {
    (
        candidate.newest,
        &candidate.display,
        candidate.kind,
        &candidate.cache,
        &candidate.target,
    )
        .cmp(&(
            cursor.newest,
            &cursor.display,
            cursor.kind,
            &cursor.cache,
            &cursor.target,
        ))
}

fn state_matches(state: &MeasureProgress, candidate: &Candidate) -> bool {
    state.kind == candidate.kind
        && state.cache == candidate.cache
        && state.target == candidate.target
        && state.identity == candidate.identity
        && state
            .stack
            .first()
            .is_some_and(|root| root.modified_ns == candidate.target_modified_ns)
}

enum MeasureOutcome {
    Complete { bytes: u64, newest: i128 },
    Interrupted(MeasureProgress),
    Changed,
}

fn measure_resumable(
    cache: &PayloadTree,
    candidate: &Candidate,
    target_metadata: &Metadata,
    resumed: Option<MeasureProgress>,
    should_stop: &mut dyn FnMut() -> bool,
) -> io::Result<MeasureOutcome> {
    let mut progress = resumed.unwrap_or_else(|| MeasureProgress {
        kind: candidate.kind,
        cache: candidate.cache.clone(),
        target: candidate.target.clone(),
        identity: candidate.identity,
        stack: vec![MeasureFrame {
            path: Vec::new(),
            device: target_metadata.device,
            inode: target_metadata.inode,
            modified_ns: target_metadata.modified_ns,
            offset: Default::default(),
            bytes: target_metadata.allocated,
            newest: target_metadata.modified_ns,
        }],
    });
    while !progress.stack.is_empty() {
        if should_stop() {
            return Ok(MeasureOutcome::Interrupted(progress));
        }
        if progress.stack.len() > MAX_DEPTH {
            return Err(io::Error::other("directory nesting too deep"));
        }
        let frame_index = progress.stack.len() - 1;
        let (frame_path, frame_identity, frame_offset) = {
            let frame = &progress.stack[frame_index];
            (
                frame.path.clone(),
                (frame.device, frame.inode, frame.modified_ns),
                frame.offset,
            )
        };
        let mut directory = cache
            .child(OsStr::new(&candidate.target))
            .map_err(io::Error::other)?;
        for part in &frame_path {
            directory = directory
                .child(&decode_component(part)?)
                .map_err(io::Error::other)?;
        }
        let metadata = directory.metadata().map_err(io::Error::other)?;
        if (metadata.device, metadata.inode, metadata.modified_ns) != frame_identity {
            return Ok(MeasureOutcome::Changed);
        }
        let mut entries = directory.directory().entries().map_err(io::Error::other)?;
        entries.seek(frame_offset).map_err(io::Error::other)?;
        let Some(name) = entries.next() else {
            let completed = progress.stack.pop().expect("current frame");
            if let Some(parent) = progress.stack.last_mut() {
                parent.bytes = parent.bytes.saturating_add(completed.bytes);
                parent.newest = parent.newest.max(completed.newest);
            } else {
                return Ok(MeasureOutcome::Complete {
                    bytes: completed.bytes,
                    newest: completed.newest,
                });
            }
            continue;
        };
        let name = name.map_err(io::Error::other)?;
        let next_offset = entries.position().map_err(io::Error::other)?;
        let child_metadata = match directory.metadata_at(&name) {
            Ok(metadata) => metadata,
            Err(error) if error.is_not_found() => return Ok(MeasureOutcome::Changed),
            Err(error) => return Err(io::Error::other(error)),
        };
        let current = &mut progress.stack[frame_index];
        current.offset = next_offset;
        current.bytes = current.bytes.saturating_add(child_metadata.allocated);
        current.newest = current.newest.max(child_metadata.modified_ns);
        if child_metadata.kind == FileKind::Directory {
            let mut path = current.path.clone();
            path.push(encode_component(&name));
            progress.stack.push(MeasureFrame {
                path,
                device: child_metadata.device,
                inode: child_metadata.inode,
                modified_ns: child_metadata.modified_ns,
                offset: Default::default(),
                bytes: 0,
                newest: child_metadata.modified_ns,
            });
        }
    }
    Ok(MeasureOutcome::Changed)
}

fn encode_component(component: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    component
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_component(encoded: &str) -> io::Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    if encoded.len() % 2 != 0 {
        return Err(io::Error::other("invalid survey path component"));
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(pair).map_err(io::Error::other)?;
        bytes.push(u8::from_str_radix(text, 16).map_err(io::Error::other)?);
    }
    Ok(OsString::from_vec(bytes))
}

/// Remove candidates until the cap and the floor hold. Each is probed, and its
/// recency checked, at the moment of removal.
fn prune(
    survey: Survey,
    limits: Limits,
    dry_run: bool,
    now_ns: i128,
    free_space: &mut dyn FnMut() -> io::Result<u64>,
    protection: &TargetProtection,
) -> Report {
    let mut should_stop = || false;
    prune_with_progress(
        survey,
        limits,
        dry_run,
        now_ns,
        free_space,
        protection,
        &mut should_stop,
        None,
    )
}

fn prune_with_progress(
    survey: Survey,
    limits: Limits,
    dry_run: bool,
    now_ns: i128,
    free_space: &mut dyn FnMut() -> io::Result<u64>,
    protection: &TargetProtection,
    should_stop: &mut dyn FnMut() -> bool,
    mut snapshot: Option<&mut SurveySnapshot>,
) -> Report {
    let Survey {
        mut report,
        lane_root,
        shared_root,
        candidates,
        mut total,
        mut free,
        more_candidates,
        complete: _,
    } = survey;
    report.more_candidates = more_candidates;
    if dry_run && more_candidates {
        report.measurement_skipped = true;
    }
    let grace = grace_ns(limits);
    let start = snapshot
        .as_deref()
        .and_then(|snapshot| snapshot.prune_cursor.as_ref())
        .and_then(|cursor| {
            candidates.iter().position(|candidate| {
                let ordering = compare_candidate_cursor(candidate, cursor);
                ordering.is_gt() || (cursor.inclusive && ordering.is_eq())
            })
        })
        .unwrap_or_else(|| {
            if snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.prune_cursor.as_ref())
                .is_some()
            {
                candidates.len()
            } else {
                0
            }
        });
    for candidate in candidates.into_iter().skip(start) {
        if total <= limits.max_bytes && free >= limits.free_floor {
            break;
        }
        if should_stop() {
            report.measurement_skipped = true;
            if let Some(snapshot) = snapshot.as_deref_mut() {
                snapshot.prune_cursor = Some(prune_cursor(&candidate, true));
            }
            report.interrupted_at = Some(candidate.display);
            break;
        }
        let tombstone = is_gc_tombstone(&candidate.target);
        if !tombstone && within_grace(candidate.newest, now_ns, grace) {
            report.recent.push(candidate.display.clone());
            advance_prune_cursor(snapshot.as_deref_mut(), &candidate);
            continue;
        }
        let root = match candidate.kind {
            CacheKind::Lane => lane_root.as_ref(),
            CacheKind::Shared => shared_root.as_ref(),
        };
        let Some(root) = root else {
            advance_prune_cursor(snapshot.as_deref_mut(), &candidate);
            continue;
        };
        let resumed = snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.rechecking.as_ref())
            .filter(|state| state_matches(state, &candidate))
            .cloned();
        match remove(
            root,
            &candidate,
            dry_run,
            now_ns,
            grace,
            protection,
            should_stop,
            resumed,
        ) {
            Ok(Outcome::Removed) => {
                if let Some(snapshot) = snapshot.as_deref_mut()
                    && snapshot
                        .rechecking
                        .as_ref()
                        .is_some_and(|state| state_matches(state, &candidate))
                {
                    snapshot.rechecking = None;
                }
                total = total.saturating_sub(candidate.bytes);
                if let Some(snapshot) = snapshot.as_deref_mut() {
                    snapshot.census_bytes = snapshot.census_bytes.saturating_sub(candidate.bytes);
                    snapshot
                        .completed
                        .retain(|saved| !same_candidate(saved, &candidate));
                }
                report.bytes = report.bytes.saturating_add(candidate.bytes);
                report.removed.push(candidate.display.clone());
                free = if dry_run {
                    free.saturating_add(candidate.bytes)
                } else {
                    free_space().unwrap_or(free.saturating_add(candidate.bytes))
                };
                advance_prune_cursor(snapshot.as_deref_mut(), &candidate);
            }
            Ok(Outcome::Busy) => {
                if let Some(snapshot) = snapshot.as_deref_mut()
                    && snapshot
                        .rechecking
                        .as_ref()
                        .is_some_and(|state| state_matches(state, &candidate))
                {
                    snapshot.rechecking = None;
                }
                report.busy.push(candidate.display.clone());
                advance_prune_cursor(snapshot.as_deref_mut(), &candidate);
            }
            Ok(Outcome::Recent(measured)) => {
                if let Some(snapshot) = snapshot.as_deref_mut()
                    && snapshot
                        .rechecking
                        .as_ref()
                        .is_some_and(|state| state_matches(state, &candidate))
                {
                    snapshot.rechecking = None;
                }
                match measured {
                    Some((bytes, newest)) => {
                        total = total.saturating_sub(candidate.bytes).saturating_add(bytes);
                        let refreshed = Candidate {
                            bytes,
                            newest,
                            ..candidate.clone()
                        };
                        if let Some(snapshot) = snapshot.as_deref_mut() {
                            snapshot.census_bytes = snapshot
                                .census_bytes
                                .saturating_sub(candidate.bytes)
                                .saturating_add(bytes);
                            if let Some(saved) = snapshot
                                .completed
                                .iter_mut()
                                .find(|saved| same_candidate(saved, &candidate))
                            {
                                saved.bytes = bytes;
                                saved.newest = newest;
                            }
                        }
                        report.recent.push(candidate.display.clone());
                        advance_prune_cursor(snapshot.as_deref_mut(), &refreshed);
                    }
                    None => {
                        report.measurement_skipped = true;
                        report.interrupted_at = Some(candidate.display.clone());
                        if let Some(snapshot) = snapshot.as_deref_mut() {
                            snapshot.completed.clear();
                            snapshot.census_bytes = 0;
                            snapshot.census_complete = false;
                            snapshot.more_candidates = false;
                            snapshot.root_cursor = None;
                            snapshot.prune_cursor = None;
                            snapshot.measuring = None;
                            snapshot.rechecking = None;
                        }
                        break;
                    }
                }
            }
            Ok(Outcome::Interrupted {
                rechecking,
                detached,
            }) => {
                if let Some(snapshot) = snapshot.as_deref_mut() {
                    if detached {
                        snapshot.completed.clear();
                        snapshot.census_bytes = 0;
                        snapshot.census_complete = false;
                        snapshot.more_candidates = false;
                        snapshot.root_cursor = None;
                        snapshot.prune_cursor = None;
                        snapshot.measuring = None;
                        snapshot.rechecking = None;
                    } else {
                        snapshot.rechecking = rechecking;
                        snapshot.prune_cursor = Some(prune_cursor(&candidate, true));
                    }
                }
                report.measurement_skipped = true;
                report.interrupted_at = Some(candidate.display);
                break;
            }
            Err(error) => {
                if let Some(snapshot) = snapshot.as_deref_mut()
                    && snapshot
                        .rechecking
                        .as_ref()
                        .is_some_and(|state| state_matches(state, &candidate))
                {
                    snapshot.rechecking = None;
                }
                report.busy.push(candidate.display.clone());
                report
                    .errors
                    .push(format!("{}: {error}", candidate.display.display()));
                advance_prune_cursor(snapshot.as_deref_mut(), &candidate);
            }
        }
    }
    report.cache_bytes = total;
    report
}

fn prune_cursor(candidate: &Candidate, inclusive: bool) -> PruneCursor {
    PruneCursor {
        kind: candidate.kind,
        cache: candidate.cache.clone(),
        target: candidate.target.clone(),
        newest: candidate.newest,
        display: candidate.display.clone(),
        inclusive,
    }
}

fn advance_prune_cursor(snapshot: Option<&mut SurveySnapshot>, candidate: &Candidate) {
    let Some(snapshot) = snapshot else { return };
    let candidate_cursor = prune_cursor(candidate, false);
    match snapshot.prune_cursor.as_ref() {
        Some(current) if compare_candidate_cursor(candidate, current).is_gt() => {
            snapshot.prune_cursor = Some(candidate_cursor);
        }
        Some(current)
            if compare_candidate_cursor(candidate, current).is_eq() && current.inclusive =>
        {
            snapshot.prune_cursor = Some(candidate_cursor);
        }
        None => snapshot.prune_cursor = Some(candidate_cursor),
        _ => {}
    }
}

fn grace_ns(limits: Limits) -> i128 {
    i128::from(limits.grace_seconds).saturating_mul(1_000_000_000)
}

/// A write at or after `now - grace` is recent. One dated more than `FUTURE_SKEW`
/// ahead of now comes from a wrong clock and is not.
fn within_grace(newest: i128, now_ns: i128, grace: i128) -> bool {
    let age = now_ns.saturating_sub(newest);
    age < grace && age > -FUTURE_SKEW_NS
}

/// Managed target directories in one of the host's private or shared caches.
fn discover(
    root: &Directory,
    cache_root: &Path,
    kind: CacheKind,
    report: &mut Report,
    budget: &mut MeasureBudget,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let names = match provenant_os::fs::entry_names(
        root,
        if budget.preview {
            PREVIEW_ENTRIES
        } else {
            MAX_ENTRIES
        },
    ) {
        Ok(names) => names,
        Err(error) => {
            report.errors.push(error.to_string());
            return candidates;
        }
    };
    for name in names {
        if budget.preview_limited() {
            break;
        }
        let Some(digest) = name.to_str().filter(|name| is_cache_digest(name)) else {
            continue;
        };
        if !budget.consume() {
            break;
        }
        let Ok(metadata) = root.metadata_at(&name, false) else {
            continue;
        };
        if metadata.kind != FileKind::Directory
            || metadata.uid != effective_uid()
            || (kind == CacheKind::Lane && metadata.mode & 0o7777 != 0o700)
            || (kind == CacheKind::Shared && metadata.mode & 0o022 != 0)
        {
            continue;
        }
        let Ok(cache) = PayloadTree::claim(root, &name, &metadata) else {
            continue;
        };
        for (target_name, logical_name) in TARGETS
            .into_iter()
            .map(|name| (name, name))
            .chain(GC_TOMBSTONES)
        {
            let target = OsStr::new(target_name);
            let metadata = match cache.metadata_at(target) {
                Ok(metadata) if metadata.kind == FileKind::Directory => metadata,
                Ok(_) => continue,
                Err(error) if error.is_not_found() => continue,
                Err(error) => {
                    report.errors.push(error.to_string());
                    continue;
                }
            };
            let Ok(target_tree) = cache.child(target) else {
                continue;
            };
            let display = cache_root.join(digest).join(logical_name);
            match measure(&target_tree, 0, budget) {
                Ok((bytes, newest)) => candidates.push(Candidate {
                    kind,
                    cache: digest.to_owned(),
                    target: target_name.to_owned(),
                    display,
                    bytes,
                    newest: if target_name == logical_name {
                        newest
                    } else {
                        i128::MIN
                    },
                    target_modified_ns: metadata.modified_ns,
                    identity: (metadata.device, metadata.inode),
                }),
                Err(error) => report
                    .errors
                    .push(format!("{}: {error}", display.display())),
            }
        }
    }
    candidates
}

fn directory_names(
    directory: &PayloadTree,
    budget: &mut MeasureBudget,
) -> io::Result<Vec<OsString>> {
    let limit = if budget.preview {
        budget.remaining.min(PREVIEW_ENTRIES)
    } else {
        MAX_ENTRIES
    };
    let mut names = Vec::new();
    for entry in
        provenant_os::fs::entry_names(directory.directory(), limit).map_err(io::Error::other)?
    {
        if !budget.consume() {
            break;
        }
        names.push(entry);
    }
    Ok(names)
}

/// Allocated bytes and the newest modification time below `directory`.
fn measure(
    directory: &PayloadTree,
    depth: usize,
    budget: &mut MeasureBudget,
) -> io::Result<(u64, i128)> {
    if depth >= MAX_DEPTH {
        return Err(io::Error::other("directory nesting too deep"));
    }
    let metadata = directory.metadata().map_err(io::Error::other)?;
    let mut bytes = if depth == 0 { metadata.allocated } else { 0 };
    let mut newest = metadata.modified_ns;
    for name in directory_names(directory, budget)? {
        if budget.preview_limited() {
            return Ok((bytes, i128::MAX));
        }
        let metadata = directory.metadata_at(&name).map_err(io::Error::other)?;
        bytes = bytes.saturating_add(metadata.allocated);
        newest = newest.max(metadata.modified_ns);
        if metadata.kind == FileKind::Directory {
            let child = directory.child(&name).map_err(io::Error::other)?;
            let (child_bytes, child_newest) = measure(&child, depth + 1, budget)?;
            bytes = bytes.saturating_add(child_bytes);
            newest = newest.max(child_newest);
        }
    }
    Ok((bytes, newest))
}

/// Re-check target identity and recency, then remove the whole target only if
/// custody and every Cargo profile lock under the cache are free.
fn remove(
    root: &Directory,
    candidate: &Candidate,
    dry_run: bool,
    now_ns: i128,
    grace: i128,
    protection: &TargetProtection,
    should_stop: &mut dyn FnMut() -> bool,
    resumed: Option<MeasureProgress>,
) -> io::Result<Outcome> {
    let metadata: Metadata = root
        .metadata_at(OsStr::new(&candidate.cache), false)
        .map_err(io::Error::other)?;
    let cache = PayloadTree::claim(root, OsStr::new(&candidate.cache), &metadata)
        .map_err(io::Error::other)?;
    let owner = if candidate.kind == CacheKind::Lane {
        provenant_confinement::cache::lane_cargo_cache_owner_path(cache.directory())
            .map_err(io::Error::other)?
    } else {
        None
    };
    if (candidate.kind == CacheKind::Lane && owner.is_none())
        || protection.protects(
            candidate.kind,
            OsStr::new(&candidate.cache),
            owner.as_deref(),
        )
    {
        return Ok(Outcome::Busy);
    }
    let interrupted = std::cell::Cell::new(false);
    let guards = {
        let should_stop = std::cell::RefCell::new(&mut *should_stop);
        let keep_going = || {
            let stop = (*should_stop.borrow_mut())();
            interrupted.set(interrupted.get() || stop);
            !interrupted.get()
        };
        provenant_confinement::cache::lock_cargo_profiles(cache.directory(), &keep_going)
    };
    let Some(_guards) = guards else {
        return Ok(if interrupted.get() {
            Outcome::Interrupted {
                rechecking: None,
                detached: false,
            }
        } else {
            Outcome::Busy
        });
    };
    let name = OsStr::new(&candidate.target);
    let target_metadata = cache.metadata_at(name).map_err(io::Error::other)?;
    if target_metadata.kind != FileKind::Directory
        || (target_metadata.device, target_metadata.inode) != candidate.identity
    {
        return Ok(Outcome::Busy);
    }
    let target = cache.child(name).map_err(io::Error::other)?;
    if dry_run {
        return Ok(Outcome::Removed);
    }
    if is_gc_tombstone(&candidate.target) {
        let mut remaining = MAX_ENTRIES;
        if !remove_contents_until(&target, 0, &mut remaining, should_stop)? {
            return Ok(Outcome::Interrupted {
                rechecking: None,
                detached: true,
            });
        }
        drop(target);
        cache.rmdir(name).map_err(io::Error::other)?;
        return Ok(Outcome::Removed);
    }

    let (bytes, newest) =
        match measure_resumable(&cache, candidate, &target_metadata, resumed, should_stop)? {
            MeasureOutcome::Complete { bytes, newest } => (bytes, newest),
            MeasureOutcome::Interrupted(state) => {
                return Ok(Outcome::Interrupted {
                    rechecking: Some(state),
                    detached: false,
                });
            }
            MeasureOutcome::Changed => return Ok(Outcome::Recent(None)),
        };
    if bytes != candidate.bytes || newest != candidate.newest || within_grace(newest, now_ns, grace)
    {
        return Ok(Outcome::Recent(Some((bytes, newest))));
    }
    if should_stop() {
        return Ok(Outcome::Interrupted {
            rechecking: None,
            detached: false,
        });
    }
    drop(target);
    let tombstone = gc_tombstone_for(&candidate.target)
        .ok_or_else(|| io::Error::other("unknown Cargo target name"))?;
    match cache.metadata_at(OsStr::new(tombstone)) {
        Ok(_) => return Ok(Outcome::Busy),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(io::Error::other(error)),
    }
    cache
        .directory()
        .rename_noreplace(name, cache.directory(), OsStr::new(tombstone))
        .map_err(io::Error::other)?;
    let tombstone = cache
        .child(OsStr::new(tombstone))
        .map_err(io::Error::other)?;
    let mut remaining = MAX_ENTRIES;
    if !remove_contents_until(&tombstone, 0, &mut remaining, should_stop)? {
        return Ok(Outcome::Interrupted {
            rechecking: None,
            detached: true,
        });
    }
    drop(tombstone);
    let tombstone_name = gc_tombstone_for(&candidate.target)
        .ok_or_else(|| io::Error::other("unknown Cargo target name"))?;
    cache
        .rmdir(OsStr::new(tombstone_name))
        .map_err(io::Error::other)?;
    Ok(Outcome::Removed)
}

fn gc_tombstone_for(target: &str) -> Option<&'static str> {
    GC_TOMBSTONES
        .iter()
        .find(|(_, logical)| *logical == target)
        .map(|(tombstone, _)| *tombstone)
}

fn is_gc_tombstone(target: &str) -> bool {
    GC_TOMBSTONES
        .iter()
        .any(|(tombstone, _)| *tombstone == target)
}

/// Remove a target tree while its Cargo profile locks are held. Links are
/// unlinked, never followed. A rename keeps interrupted deletion out of Cargo's
/// target path; the next sweep continues from the surviving tombstone.
fn remove_contents_until(
    directory: &PayloadTree,
    depth: usize,
    remaining: &mut usize,
    should_stop: &mut dyn FnMut() -> bool,
) -> io::Result<bool> {
    if depth >= MAX_DEPTH {
        return Err(io::Error::other("directory nesting too deep"));
    }
    if should_stop() {
        return Ok(false);
    }
    directory.make_writable().map_err(io::Error::other)?;
    let mut entries = directory.directory().entries().map_err(io::Error::other)?;
    while let Some(entry) = entries.next() {
        if should_stop() {
            return Ok(false);
        }
        if *remaining == 0 {
            return Err(io::Error::other("target entry limit exceeded"));
        }
        *remaining -= 1;
        let name = entry.map_err(io::Error::other)?;
        let metadata = directory.metadata_at(&name).map_err(io::Error::other)?;
        if metadata.kind == FileKind::Directory {
            let child = directory.child(&name).map_err(io::Error::other)?;
            if !remove_contents_until(&child, depth + 1, remaining, should_stop)? {
                return Ok(false);
            }
            drop(child);
            directory.rmdir(&name).map_err(io::Error::other)?;
        } else {
            directory.unlink(&name).map_err(io::Error::other)?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::Clock;
    use provenant_os::fs::{ForeignLock, LockGuard, LockMode};
    use std::{
        fs,
        os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    };

    const TEST_NOW_SECONDS: u64 = 100_000;

    fn fixed_time() -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(TEST_NOW_SECONDS)
    }

    struct Fixture {
        home: tempfile::TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let home = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
            fs::create_dir_all(cache_root(home.path())).unwrap();
            Self { home }
        }
        fn cache(&self, digit: char) -> PathBuf {
            let path = cache_root(self.home.path()).join(digit.to_string().repeat(64));
            if !path.exists() {
                fs::create_dir(&path).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                let mut owner = b"provenant-lane-cargo-owner-v2\n".to_vec();
                owner.extend_from_slice(&0_u64.to_le_bytes());
                owner.extend_from_slice(&0_u64.to_le_bytes());
                owner.extend_from_slice(
                    Path::new(&format!("/fixture/worktree-{digit}"))
                        .as_os_str()
                        .as_bytes(),
                );
                fs::write(path.join(".provenant-worktree-owner"), owner).unwrap();
            }
            path
        }
        fn shared_cache(&self, digit: char) -> PathBuf {
            let path = shared_cache_root(self.home.path()).join(digit.to_string().repeat(64));
            if !path.exists() {
                fs::create_dir_all(&path).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            path
        }
        /// A managed target directory with `bytes` of payload and a last-use
        /// time `age` seconds ago.
        fn profile(&self, digit: char, _name: &str, bytes: usize, age: u64) -> PathBuf {
            let dir = self.cache(digit).join("target");
            fs::create_dir_all(dir.join("deps")).unwrap();
            for lock in LOCKS {
                fs::write(dir.join(lock), b"").unwrap();
            }
            let payload = dir.join("deps/payload");
            fs::write(&payload, vec![1; bytes]).unwrap();
            let when = fixed_time() - std::time::Duration::from_secs(age);
            for path in [
                &payload,
                &dir.join("deps"),
                &dir,
                &dir.join(CARGO_LOCK),
                &dir.join(LOCKS[1]),
                &dir.join(LOCKS[2]),
            ] {
                fs::File::open(path).unwrap().set_modified(when).unwrap();
            }
            dir
        }
        fn shared_profile(&self, digit: char, _name: &str, bytes: usize, age: u64) -> PathBuf {
            let dir = self.shared_cache(digit).join("target");
            fs::create_dir_all(dir.join("deps")).unwrap();
            for lock in LOCKS {
                fs::write(dir.join(lock), b"").unwrap();
            }
            let payload = dir.join("deps/payload");
            fs::write(&payload, vec![1; bytes]).unwrap();
            let when = fixed_time() - std::time::Duration::from_secs(age);
            for path in [
                &payload,
                &dir.join("deps"),
                &dir,
                &dir.join(CARGO_LOCK),
                &dir.join(LOCKS[1]),
                &dir.join(LOCKS[2]),
            ] {
                fs::File::open(path).unwrap().set_modified(when).unwrap();
            }
            dir
        }
    }
    fn plenty() -> io::Result<u64> {
        Ok(u64::MAX)
    }
    /// No grace: the fixtures' ages are minutes, and the grace has its own tests.
    fn limits(max_bytes: u64, free_floor: u64) -> Limits {
        Limits {
            max_bytes,
            free_floor,
            grace_seconds: 0,
        }
    }
    fn graced(max_bytes: u64, grace_seconds: u64) -> Limits {
        Limits {
            grace_seconds,
            ..limits(max_bytes, 0)
        }
    }
    fn now() -> i128 {
        i128::from(TEST_NOW_SECONDS) * 1_000_000_000
    }
    fn hold(dir: &Path, name: &str) -> LockGuard {
        let directory = Directory::open_anchor(dir).unwrap();
        match directory
            .try_flock_foreign(OsStr::new(name), LockMode::Exclusive)
            .unwrap()
        {
            ForeignLock::Acquired(guard) => guard,
            other => panic!("lock {name} is not free: {other:?}"),
        }
    }

    #[test]
    fn dry_run_reports_profile_size_and_budget_without_changes() {
        let fixture = Fixture::new();
        let profile = fixture.profile('a', "debug", 4096, 100);
        let budget = provenant_config::CargoCacheBudget {
            bytes: 1024,
            setting: "1K".into(),
            source: provenant_records::LimitSource::Default,
        };
        let report = collect_with_protection(
            fixture.home.path(),
            fixture.home.path(),
            limits(1024, 0),
            true,
            &budget,
            &TargetProtection::default(),
        )
        .unwrap();
        assert!(!report.measurement_skipped);
        assert_eq!(report.removed, vec![profile.clone()]);
        assert!(report.bytes >= 4096);
        assert_eq!(report.budget_bytes, 1024);
        assert_eq!(report.budget_setting, "1K");
        assert_eq!(report.budget_source, "default");
        assert!(profile.join("deps/payload").is_file());
    }

    #[test]
    fn a_warm_launch_moves_a_target_to_the_mru_end_without_artifact_writes() {
        let fixture = Fixture::new();
        let older = fixture.profile('a', "debug", 4096, 3000);
        let other = fixture.profile('b', "debug", 4096, 1000);
        let payload = older.join("deps/payload");
        let before = fs::metadata(&payload).unwrap().modified().unwrap();
        let root = Directory::open_anchor(&cache_root(fixture.home.path())).unwrap();
        let root_before = root.metadata().unwrap().modified_ns;

        assert!(mark_managed_target_use(fixture.home.path(), &older).unwrap());
        let after = fs::metadata(&payload).unwrap().modified().unwrap();
        assert_eq!(before, after);
        assert!(
            cache_root(fixture.home.path())
                .join(CACHE_ACTIVITY_MARKER)
                .is_file()
        );
        assert_ne!(root_before, root.metadata().unwrap().modified_ns);

        let mut free = plenty;
        let survey = survey(fixture.home.path(), &mut free, false);
        assert_eq!(survey.candidates.last().unwrap().display, older);
        assert!(other.exists());
    }

    #[test]
    fn an_interrupted_large_target_census_resumes_from_its_epoch_sidecar() {
        let fixture = Fixture::new();
        let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = fixture.profile('a', "debug", 1, 100);
        let large = target.join("many");
        fs::create_dir(&large).unwrap();
        for index in 0..128 {
            fs::write(
                large.join(format!("file-{index:03}")),
                vec![index as u8; 1024],
            )
            .unwrap();
        }
        let budget = provenant_config::CargoCacheBudget {
            bytes: u64::MAX,
            setting: "100%".into(),
            source: provenant_records::LimitSource::Default,
        };
        let mut calls = 0;
        let mut stop = || {
            calls += 1;
            calls > 24
        };
        let mut refresh = TargetProtection::default;
        let partial = collect_with_progress(
            state.path(),
            fixture.home.path(),
            limits(u64::MAX, 0),
            false,
            &budget,
            &TargetProtection::default(),
            None,
            "0000000000000001",
            &mut stop,
            &mut refresh,
        )
        .unwrap();
        assert!(partial.measurement_skipped);
        assert_eq!(partial.interrupted_at.as_deref(), Some(target.as_path()));
        let detail = partial
            .cursor_detail
            .clone()
            .expect("published continuation");
        assert!(target.exists());
        assert!(
            state
                .path()
                .join("tick/gc-cache-surveys")
                .join(detail)
                .is_file()
        );

        let mut continue_scan = || false;
        let mut refresh = TargetProtection::default;
        let resumed = collect_with_progress(
            state.path(),
            fixture.home.path(),
            limits(u64::MAX, 0),
            false,
            &budget,
            &TargetProtection::default(),
            partial.cursor_detail.as_deref(),
            "0000000000000002",
            &mut continue_scan,
            &mut refresh,
        )
        .unwrap();
        assert!(!resumed.measurement_skipped);
        assert!(resumed.cache_bytes >= 128 * 1024);
        assert!(target.exists());
    }

    #[test]
    fn survey_snapshot_cleanup_keeps_the_active_epoch_and_removes_stale_ones() {
        let host = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(host.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(host.path().join("tick")).unwrap();
        fs::set_permissions(host.path().join("tick"), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = host.path().join("tick/gc-cache-surveys");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let active = format!("{SURVEY_SNAPSHOT_PREFIX}0000000000000001.json");
        let unrelated = format!("{SURVEY_SNAPSHOT_PREFIX}invalid.json");
        fs::write(directory.join(&active), b"active").unwrap();
        fs::write(directory.join(&unrelated), b"unrelated").unwrap();
        let stale_names = (2..=130_u64)
            .map(|epoch| format!("{SURVEY_SNAPSHOT_PREFIX}{epoch:016x}.json"))
            .collect::<Vec<_>>();
        for stale in &stale_names {
            fs::write(directory.join(stale), b"stale").unwrap();
        }

        let first = prune_stale_survey_snapshots(host.path(), Some(&active));

        assert!(
            first
                .iter()
                .any(|warning| warning.contains("cleanup deferred"))
        );
        assert!(directory.join(&active).is_file());
        assert!(directory.join(&unrelated).is_file());
        assert!(
            stale_names
                .iter()
                .any(|stale| directory.join(stale).exists())
        );

        let second = prune_stale_survey_snapshots(host.path(), Some(&active));

        assert!(second.is_empty());
        assert!(
            stale_names
                .iter()
                .all(|stale| !directory.join(stale).exists())
        );
        assert!(directory.join(&active).is_file());
        assert!(directory.join(&unrelated).is_file());
    }

    #[test]
    fn compact_survey_inventory_keeps_more_than_fifty_thousand_cache_keys_bounded() {
        let candidates = (0..100_002)
            .map(|index| Candidate {
                kind: CacheKind::Lane,
                cache: format!("{:064x}", index / 2),
                target: if index % 2 == 0 {
                    "target".into()
                } else {
                    "target-lint".into()
                },
                display: PathBuf::new(),
                bytes: index as u64,
                newest: 1_700_000_000_000_000_000,
                target_modified_ns: 1_700_000_000_000_000_000,
                identity: (1_234_567, index as u64),
            })
            .collect::<Vec<_>>();
        let snapshot = SurveySnapshot {
            version: SURVEY_SNAPSHOT_VERSION,
            epoch: "0000000000000001".into(),
            completed: candidates,
            ..SurveySnapshot::default()
        };
        let mut bounded = BoundedSidecar::new(SURVEY_SNAPSHOT_LIMIT as usize);

        serde_json::to_writer(&mut bounded, &snapshot).unwrap();
        let encoded = bounded.bytes;
        let decoded: SurveySnapshot = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(decoded.completed.len(), 100_002);
        assert!(encoded.len() < SURVEY_SNAPSHOT_LIMIT as usize);
    }

    #[test]
    fn interrupted_dry_run_preview_does_not_write_resume_state() {
        let fixture = Fixture::new();
        let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = fixture.profile('a', "debug", 1, 100);
        let budget = provenant_config::CargoCacheBudget {
            bytes: 0,
            setting: "0".into(),
            source: provenant_records::LimitSource::Default,
        };
        let mut calls = 0;
        let mut stop = || {
            calls += 1;
            calls > 5
        };
        let mut refresh = TargetProtection::default;

        let report = collect_with_progress(
            state.path(),
            fixture.home.path(),
            limits(0, 0),
            true,
            &budget,
            &TargetProtection::default(),
            None,
            "0000000000000001",
            &mut stop,
            &mut refresh,
        )
        .unwrap();

        assert!(report.measurement_skipped);
        assert!(report.cursor_detail.is_none());
        assert!(target.exists());
        assert!(!state.path().join("tick").exists());
    }

    #[test]
    fn dry_run_uses_refreshed_shared_target_custody_for_reclaim_preview() {
        let fixture = Fixture::new();
        let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = fixture.shared_profile('a', "debug", 4096, 100);
        let budget = provenant_config::CargoCacheBudget {
            bytes: 0,
            setting: "0".into(),
            source: provenant_records::LimitSource::Default,
        };
        let mut stop = || false;
        let mut refresh = || TargetProtection {
            shared_projects: ["a".repeat(64)].into(),
            ..TargetProtection::default()
        };

        let report = collect_with_progress(
            state.path(),
            fixture.home.path(),
            limits(0, 0),
            true,
            &budget,
            &TargetProtection::default(),
            None,
            "0000000000000001",
            &mut stop,
            &mut refresh,
        )
        .unwrap();

        assert!(report.removed.is_empty());
        assert!(report.busy.iter().any(|path| path == &target));
        assert!(target.join("deps/payload").is_file());
        assert!(!state.path().join("tick").exists());
    }

    #[test]
    fn registered_writer_without_run_store_protects_its_shared_target()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{CancelToken, GitOid, WaitBudget};
        use provenant_os::exec::SystemExecutor;
        use provenant_os::fs::{Directory, LockMode};
        use std::process::Command;

        let fixture = Fixture::new();
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let project = root.join("project");
        let home = root.join("home");
        let state_path = root.join("state");
        fs::create_dir(&project)?;
        fs::create_dir(&home)?;
        fs::create_dir(&state_path)?;
        fs::set_permissions(&state_path, fs::Permissions::from_mode(0o700))?;
        let git = provenant_os::git::git_executable().ok_or("git executable unavailable")?;
        let run_git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let output = Command::new(&git)
                .env_clear()
                .envs(provenant_os::fixture::fixture_git_settings())
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(&project)
                .args(args)
                .output()?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
            }
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        run_git(&["init", "-q", "-b", "main"])?;
        run_git(&["config", "user.name", "fixture"])?;
        run_git(&["config", "user.email", "fixture@example.invalid"])?;
        run_git(&["commit", "--allow-empty", "-qm", "initial"])?;
        let common_dir = project.join(".git");
        let mut root_inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        root_inputs.state_dir = Some(state_path.clone());
        let roots = root_inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::from_verified_primary_checkout(
                common_dir.clone(),
                project.clone(),
            )?,
        )?;

        let runtime =
            provenant_confinement::UtilityRuntime::read_only(&git, roots.state.as_path())?;
        let executor = SystemExecutor::new(1)?;
        let cancel = CancelToken::new();
        let utility_budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let repo = runtime.open_repo(&executor, &project, utility_budget, &cancel)?;
        let worktree = provenant_confinement::verify_worktree(&repo, &project)?;
        let inventory = provenant_confinement::worktrees::RepoFacts::default().writer_inventory(
            &repo,
            &executor,
            &runtime,
            utility_budget,
            &cancel,
        )?;
        let projects_state = roots.state.as_path().join("worktrees");
        fs::create_dir_all(&projects_state)?;
        let project_state_path = projects_state.join(binding.project_id().to_string());
        fs::create_dir_all(&project_state_path)?;
        let project_state = Directory::open_anchor(&project_state_path)?;
        let base = GitOid::parse(&run_git(&["rev-parse", "HEAD"])?)?;
        let (slot, _) = provenant_confinement::worktrees::writer_custody(
            &project_state,
            &worktree,
            &project,
            &provenant_confinement::repo::GitRefName::branch("main")?,
            base,
            || Ok(inventory.clone()),
        )?;
        let clock = provenant_os::clock::SystemClock::new();
        let _writer = slot.lock(
            OsStr::new("writer.lock"),
            LockMode::Exclusive,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?;

        // No RunStore or attempt record is created: the writer-slot lock is the
        // only durable live-owner evidence for this registered project.
        let key = crate::target_seed::project_key(&common_dir);
        let target = shared_cache_root(fixture.home.path())
            .join(&key)
            .join("target");
        fs::create_dir_all(target.join("deps"))?;
        fs::write(target.join(CARGO_LOCK), b"")?;
        fs::write(target.join("deps/payload"), vec![7_u8; 8192])?;

        let protection = target_protection_for_launch(&roots, None, None);
        assert!(
            !protection.unobserved,
            "all registered writer custody was observed"
        );
        assert!(protection.shared_projects.contains(&key));
        let budget = provenant_config::CargoCacheBudget {
            bytes: 0,
            setting: "0".into(),
            source: provenant_records::LimitSource::Default,
        };
        let report = collect_with_protection(
            roots.state.as_path(),
            fixture.home.path(),
            limits(0, 0),
            false,
            &budget,
            &protection,
        )?;
        assert!(report.removed.is_empty());
        assert!(report.busy.iter().any(|path| path == &target));
        assert!(target.join("deps/payload").is_file());
        Ok(())
    }

    #[test]
    fn an_interrupted_cache_root_walk_resumes_after_completed_keys() {
        let fixture = Fixture::new();
        let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        const CACHE_KEYS: usize = 16;
        for index in 0..CACHE_KEYS {
            let digit = char::from_digit(index as u32, 16).unwrap();
            fixture.profile(digit, "debug", 1, 100);
        }
        let budget = provenant_config::CargoCacheBudget {
            bytes: u64::MAX,
            setting: "100%".into(),
            source: provenant_records::LimitSource::Default,
        };
        let mut detail = None;
        for epoch in 1..=16_u64 {
            let mut calls = 0;
            let mut stop = || {
                calls += 1;
                calls > 18
            };
            let mut refresh = TargetProtection::default;
            let report = collect_with_progress(
                state.path(),
                fixture.home.path(),
                limits(u64::MAX, 0),
                false,
                &budget,
                &TargetProtection::default(),
                detail.as_deref(),
                &format!("{epoch:016x}"),
                &mut stop,
                &mut refresh,
            )
            .unwrap();
            if !report.measurement_skipped {
                assert!(report.cache_bytes >= (CACHE_KEYS as u64) * 4096);
                return;
            }
            detail = report.cursor_detail;
            assert!(detail.is_some(), "interrupted census must publish progress");
        }
        panic!("flat cache-root census failed to reach later cache keys");
    }

    #[test]
    fn cache_root_with_more_than_twenty_thousand_entries_is_collectable() {
        let fixture = Fixture::new();
        let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        const CACHE_KEYS: usize = 20_001;
        let root = cache_root(fixture.home.path());
        let target = root.join(format!("{:064x}", CACHE_KEYS - 1)).join("target");
        for index in 0..CACHE_KEYS {
            let cache = root.join(format!("{index:064x}"));
            fs::create_dir(&cache).unwrap();
            fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::create_dir_all(target.join("deps")).unwrap();
        fs::write(target.join(CARGO_LOCK), b"").unwrap();
        fs::write(target.join("deps/payload"), vec![9_u8; 128]).unwrap();

        let budget = provenant_config::CargoCacheBudget {
            bytes: u64::MAX,
            setting: "100%".into(),
            source: provenant_records::LimitSource::Default,
        };
        let mut continue_scan = || false;
        let mut refresh = TargetProtection::default;
        let report = collect_with_progress(
            state.path(),
            fixture.home.path(),
            limits(u64::MAX, 0),
            false,
            &budget,
            &TargetProtection::default(),
            None,
            "0000000000000001",
            &mut continue_scan,
            &mut refresh,
        )
        .unwrap();

        assert!(!report.measurement_skipped);
        assert!(report.cache_bytes >= 128);
        assert!(target.exists());
    }

    #[test]
    fn one_sweep_prunes_across_bounded_lru_batches() {
        let fixture = Fixture::new();
        let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        const CACHE_KEYS: usize = MAX_LRU_BATCH + 1;
        let root = cache_root(fixture.home.path());
        let mut targets = Vec::with_capacity(CACHE_KEYS);
        for index in 0..CACHE_KEYS {
            let cache = root.join(format!("{index:064x}"));
            fs::create_dir(&cache).unwrap();
            fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).unwrap();
            let mut owner = b"provenant-lane-cargo-owner-v2\n".to_vec();
            owner.extend_from_slice(&0_u64.to_le_bytes());
            owner.extend_from_slice(&0_u64.to_le_bytes());
            owner.extend_from_slice(
                Path::new(&format!("/fixture/worktree-{index}"))
                    .as_os_str()
                    .as_bytes(),
            );
            fs::write(cache.join(".provenant-worktree-owner"), owner).unwrap();
            let target = cache.join("target");
            fs::create_dir_all(target.join("deps")).unwrap();
            for lock in LOCKS {
                fs::write(target.join(lock), b"").unwrap();
            }
            fs::write(target.join("deps/payload"), vec![1_u8; 128]).unwrap();
            targets.push(target);
        }

        let budget = provenant_config::CargoCacheBudget {
            bytes: 0,
            setting: "0".into(),
            source: provenant_records::LimitSource::Default,
        };
        let mut continue_scan = || false;
        let mut refresh = TargetProtection::default;
        let report = collect_with_progress(
            state.path(),
            fixture.home.path(),
            limits(0, 0),
            false,
            &budget,
            &TargetProtection::default(),
            None,
            "0000000000000002",
            &mut continue_scan,
            &mut refresh,
        )
        .unwrap();

        assert_eq!(report.removed.len(), CACHE_KEYS);
        assert_eq!(report.cache_bytes, 0);
        assert!(targets.iter().all(|target| !target.exists()));
    }

    #[test]
    fn prune_resume_cursor_advances_past_busy_targets() {
        let fixture = Fixture::new();
        for digit in ['a', 'b', 'c', 'd'] {
            fixture.profile(digit, "debug", 4096, 100);
        }
        let mut free = plenty;
        let first_survey = survey(fixture.home.path(), &mut free, false);
        let expected = first_survey
            .candidates
            .iter()
            .map(|candidate| candidate.display.clone())
            .collect::<Vec<_>>();
        let mut snapshot = SurveySnapshot {
            version: SURVEY_SNAPSHOT_VERSION,
            completed: first_survey.candidates.clone(),
            ..SurveySnapshot::default()
        };
        let protection = TargetProtection {
            unobserved: true,
            ..TargetProtection::default()
        };
        let mut calls = 0;
        let mut stop_after_two = || {
            calls += 1;
            calls > 2
        };
        let first = prune_with_progress(
            first_survey,
            limits(0, 0),
            false,
            now(),
            &mut free,
            &protection,
            &mut stop_after_two,
            Some(&mut snapshot),
        );
        assert_eq!(first.interrupted_at.as_ref(), expected.get(2));

        let second_survey = survey(fixture.home.path(), &mut free, false);
        let mut calls = 0;
        let mut stop_after_one = || {
            calls += 1;
            calls > 1
        };
        let second = prune_with_progress(
            second_survey,
            limits(0, 0),
            false,
            now(),
            &mut free,
            &protection,
            &mut stop_after_one,
            Some(&mut snapshot),
        );
        assert_eq!(second.interrupted_at.as_ref(), expected.get(3));
        assert!(second.removed.is_empty());
    }

    #[test]
    fn an_interrupted_target_prune_resumes_from_its_hidden_tombstone() {
        let fixture = Fixture::new();
        let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = fixture.profile('a', "debug", 8192, 100);
        for index in 0..64 {
            fs::write(
                target.join(format!("artifact-{index:03}")),
                vec![index as u8; 4096],
            )
            .unwrap();
        }
        let state_root = state.path();
        let budget = provenant_config::CargoCacheBudget {
            bytes: 0,
            setting: "0".into(),
            source: provenant_records::LimitSource::Default,
        };
        let mut began_deletion = false;
        let mut stop_at_tombstone = || {
            if !target.exists() && !began_deletion {
                began_deletion = true;
                true
            } else {
                false
            }
        };
        let mut refresh = TargetProtection::default;
        let partial = collect_with_progress(
            state_root,
            fixture.home.path(),
            limits(0, 0),
            false,
            &budget,
            &TargetProtection::default(),
            None,
            "0000000000000001",
            &mut stop_at_tombstone,
            &mut refresh,
        )
        .unwrap();
        assert!(partial.measurement_skipped);
        assert_eq!(partial.interrupted_at.as_deref(), Some(target.as_path()));
        let cache = target.parent().unwrap();
        assert!(!target.exists());
        assert!(cache.join(GC_TOMBSTONES[0].0).is_dir());

        let mut continue_delete = || false;
        let mut refresh = TargetProtection::default;
        let resumed = collect_with_progress(
            state_root,
            fixture.home.path(),
            limits(0, 0),
            false,
            &budget,
            &TargetProtection::default(),
            partial.cursor_detail.as_deref(),
            "0000000000000002",
            &mut continue_delete,
            &mut refresh,
        )
        .unwrap();
        assert!(!resumed.measurement_skipped);
        assert!(resumed.removed.iter().any(|path| path == &target));
        assert!(!cache.join(GC_TOMBSTONES[0].0).exists());
    }

    #[test]
    fn an_interrupted_lock_held_recheck_resumes_before_renaming() {
        let fixture = Fixture::new();
        let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = fixture.profile('a', "debug", 8192, 100);
        for index in 0..64 {
            fs::write(
                target.join(format!("artifact-{index:03}")),
                vec![index as u8; 4096],
            )
            .unwrap();
        }
        let budget = provenant_config::CargoCacheBudget {
            bytes: 0,
            setting: "0".into(),
            source: provenant_records::LimitSource::Default,
        };
        let mut locked_steps = 0;
        let mut stop_during_recheck = || {
            let directory = Directory::open_anchor(&target).unwrap();
            match directory
                .try_flock_foreign(OsStr::new(CARGO_LOCK), LockMode::Exclusive)
                .unwrap()
            {
                ForeignLock::Held => {
                    locked_steps += 1;
                    locked_steps > 8
                }
                ForeignLock::Acquired(guard) => {
                    drop(guard);
                    false
                }
                ForeignLock::Absent => false,
            }
        };
        let mut refresh = TargetProtection::default;
        let partial = collect_with_progress(
            state.path(),
            fixture.home.path(),
            limits(0, 0),
            false,
            &budget,
            &TargetProtection::default(),
            None,
            "0000000000000001",
            &mut stop_during_recheck,
            &mut refresh,
        )
        .unwrap();
        assert!(partial.measurement_skipped);
        assert!(target.is_dir());
        assert!(!target.parent().unwrap().join(GC_TOMBSTONES[0].0).exists());

        let mut continue_recheck = || false;
        let mut refresh = TargetProtection::default;
        let resumed = collect_with_progress(
            state.path(),
            fixture.home.path(),
            limits(0, 0),
            false,
            &budget,
            &TargetProtection::default(),
            partial.cursor_detail.as_deref(),
            "0000000000000002",
            &mut continue_recheck,
            &mut refresh,
        )
        .unwrap();
        assert!(!resumed.measurement_skipped);
        assert!(resumed.removed.iter().any(|path| path == &target));
        assert!(!target.exists());
    }

    #[test]
    fn nothing_is_removed_while_under_the_cap_and_above_the_floor() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 100);
        let report = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            false,
            now(),
            &mut plenty,
        );
        assert!(report.removed.is_empty() && debug.exists());
        assert!(report.cache_bytes >= 4096);
    }

    #[test]
    fn a_held_lock_prevents_deletion_whichever_of_the_three_is_held() {
        for lock in LOCKS {
            let fixture = Fixture::new();
            let debug = fixture.profile('a', "debug", 4096, 100);
            let guard = hold(&debug, lock);
            let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
            assert!(report.removed.is_empty(), "{lock}");
            assert_eq!(report.busy, vec![debug.clone()], "{lock}");
            assert!(debug.join("deps/payload").exists(), "{lock}");
            drop(guard);
            let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
            assert_eq!(report.removed, vec![debug.clone()], "{lock}");
            assert!(!debug.exists(), "{lock}");
        }
    }

    #[test]
    fn private_and_shared_profiles_share_one_lru_budget() {
        let fixture = Fixture::new();
        let oldest = fixture.profile('a', "debug", 8192, 3000);
        let middle = fixture.shared_profile('b', "debug", 8192, 2000);
        let newest = fixture.profile('c', "debug", 8192, 1000);
        let before = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            true,
            now(),
            &mut plenty,
        );
        let mut free = plenty;
        let survey = survey(fixture.home.path(), &mut free, false);
        let oldest_bytes = survey.candidates[0].bytes;
        let report = sweep(
            fixture.home.path(),
            limits(before.cache_bytes - oldest_bytes, 0),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![oldest.clone()]);
        assert!(!oldest.exists() && middle.exists() && newest.exists());
    }

    #[test]
    fn shared_target_cargo_lock_and_live_project_protection_prevent_removal() {
        let fixture = Fixture::new();
        let debug = fixture.shared_profile('a', "debug", 4096, 100);
        let guard = hold(&debug, CARGO_LOCK);
        let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
        assert!(report.removed.is_empty());
        assert_eq!(report.busy, vec![debug.clone()]);
        drop(guard);

        let mut protection = TargetProtection::default();
        protection.shared_projects.insert("a".repeat(64));
        let report = {
            let mut free = plenty;
            prune(
                survey(fixture.home.path(), &mut free, false),
                limits(0, 0),
                false,
                now(),
                &mut free,
                &protection,
            )
        };
        assert!(report.removed.is_empty());
        assert_eq!(report.busy, vec![debug.clone()]);
        assert!(debug.join("deps/payload").exists());
    }

    #[test]
    fn live_private_target_owner_is_never_evicted_without_a_cargo_lock() {
        let fixture = Fixture::new();
        let target = fixture.profile('a', "debug", 4096, 100);
        let mut protection = TargetProtection::default();
        protection
            .worktrees
            .insert(PathBuf::from("/fixture/worktree-a"));
        let report = {
            let mut free = plenty;
            prune(
                survey(fixture.home.path(), &mut free, false),
                limits(0, 0),
                false,
                now(),
                &mut free,
                &protection,
            )
        };
        assert!(report.removed.is_empty());
        assert_eq!(report.busy, vec![target.clone()]);
        assert!(target.join("deps/payload").exists());
    }

    #[test]
    fn incomplete_target_payload_counts_and_is_reclaimed_with_its_target() {
        let fixture = Fixture::new();
        let target = fixture.cache('a').join("target");
        let payload = target.join("incomplete-seed/payload");
        fs::create_dir_all(payload.parent().unwrap()).unwrap();
        fs::write(&payload, vec![7; 8192]).unwrap();
        let preview = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            true,
            now(),
            &mut plenty,
        );
        assert!(preview.cache_bytes >= 8192);

        let apply = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
        assert_eq!(apply.removed, vec![target]);
        assert!(apply.cache_bytes < preview.cache_bytes);
        assert!(!payload.exists());
    }

    #[test]
    fn least_recently_used_goes_first_and_the_cap_stops_collection() {
        let fixture = Fixture::new();
        let oldest = fixture.profile('a', "debug", 8192, 3000);
        let middle = fixture.profile('b', "debug", 8192, 2000);
        let newest = fixture.profile('c', "debug", 8192, 1000);
        let before = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            true,
            now(),
            &mut plenty,
        );
        let mut free = plenty;
        let survey = survey(fixture.home.path(), &mut free, false);
        let oldest_bytes = survey.candidates[0].bytes;
        let middle_bytes = survey.candidates[1].bytes;
        // Room for two of the three: only the oldest goes.
        let report = sweep(
            fixture.home.path(),
            limits(before.cache_bytes - oldest_bytes, 0),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![oldest.clone()]);
        assert!(!oldest.exists() && middle.exists() && newest.exists());
        assert_eq!(report.bytes, oldest_bytes);
        assert!(report.cache_bytes <= before.cache_bytes - oldest_bytes);
        // A tighter cap takes the next oldest, never the newest first.
        let report = sweep(
            fixture.home.path(),
            limits(before.cache_bytes - oldest_bytes - middle_bytes, 0),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![middle.clone()]);
        assert!(newest.exists());
    }

    /// A fake free-space reader whose value follows the observed removals.
    fn free_after_removals(
        start: u64,
        bytes_per_removed_profile: u64,
        profiles: Vec<PathBuf>,
    ) -> impl FnMut() -> io::Result<u64> {
        move || {
            let removed = profiles.iter().filter(|profile| !profile.exists()).count() as u64;
            Ok(start.saturating_add(removed.saturating_mul(bytes_per_removed_profile)))
        }
    }

    #[test]
    fn the_free_space_floor_is_honoured_and_no_more_is_removed() {
        let floor = 1_000_000;
        for shortfall in [1, 5_000, 5_001, 12_000] {
            let fixture = Fixture::new();
            let profiles = [
                fixture.profile('a', "debug", 8192, 3000),
                fixture.profile('b', "debug", 8192, 2000),
                fixture.profile('c', "debug", 8192, 1000),
            ];
            let size = sweep(
                fixture.home.path(),
                limits(u64::MAX, 0),
                true,
                now(),
                &mut plenty,
            )
            .cache_bytes
                / 3;
            let per_profile = size.min(5_000);
            let mut free = free_after_removals(floor - shortfall, per_profile, profiles.to_vec());
            let report = sweep(
                fixture.home.path(),
                limits(u64::MAX, floor),
                false,
                now(),
                &mut free,
            );
            let recovered = free().unwrap();
            assert!(report.errors.is_empty());
            assert!(report.bytes > 0);
            assert!(recovered >= floor, "shortfall {shortfall}: {recovered}");
            assert!(recovered - floor < per_profile, "shortfall {shortfall}");
        }
    }

    #[test]
    fn the_cap_and_the_floor_apply_together() {
        {
            let fixture = Fixture::new();
            let oldest = fixture.profile('a', "debug", 8192, 3000);
            let middle = fixture.profile('b', "debug", 8192, 2000);
            let newest = fixture.profile('c', "debug", 8192, 1000);
            let total = sweep(
                fixture.home.path(),
                limits(u64::MAX, 0),
                true,
                now(),
                &mut plenty,
            )
            .cache_bytes;
            let mut free = free_after_removals(
                100,
                100,
                vec![oldest.clone(), middle.clone(), newest.clone()],
            );
            let report = sweep(
                fixture.home.path(),
                limits(total - 1, 200),
                false,
                now(),
                &mut free,
            );
            let recovered = free().unwrap();
            assert!(report.cache_bytes < total);
            assert!(recovered >= 200);
            assert!(recovered - 200 < 100);
            assert!(oldest.exists() || middle.exists() || newest.exists());
        }

        let fixture = Fixture::new();
        let oldest = fixture.profile('a', "debug", 8192, 3000);
        let middle = fixture.profile('b', "debug", 8192, 2000);
        let newest = fixture.profile('c', "debug", 8192, 1000);
        let mut free = free_after_removals(
            100,
            50,
            vec![oldest.clone(), middle.clone(), newest.clone()],
        );
        let report = sweep(
            fixture.home.path(),
            limits(u64::MAX, 175),
            false,
            now(),
            &mut free,
        );
        let recovered = free().unwrap();
        assert!(report.errors.is_empty());
        assert!(recovered >= 175);
        assert!(recovered - 175 < 50);
        assert!(oldest.exists() || middle.exists() || newest.exists());
    }

    #[test]
    fn a_dry_run_reports_without_deleting_or_holding_locks() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 100);
        let report = sweep(fixture.home.path(), limits(0, 0), true, now(), &mut plenty);
        assert_eq!(report.removed, vec![debug.clone()]);
        assert!(report.bytes >= 4096);
        assert!(debug.join("deps/payload").exists());
        let _free = hold(&debug, CARGO_LOCK);
    }

    #[test]
    fn whole_managed_targets_are_collected_and_other_cache_entries_are_kept() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 100);
        let doc = debug.join("doc");
        fs::create_dir_all(&doc).unwrap();
        fs::write(doc.join("index.html"), b"x").unwrap();
        let lint = fixture.cache('a').join("target-lint");
        fs::create_dir_all(&lint).unwrap();
        fs::write(lint.join(CARGO_LOCK), b"").unwrap();
        fs::write(lint.join("payload"), vec![1; 4096]).unwrap();
        let stray = fixture.cache('a').join("private-config");
        fs::create_dir_all(&stray).unwrap();
        fs::write(stray.join(CARGO_LOCK), b"").unwrap();
        let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
        assert!(!debug.exists() && !lint.exists(), "{report:?}");
        assert_eq!(report.removed.len(), 2);
        assert!(!doc.join("index.html").exists());
        assert!(stray.join(CARGO_LOCK).exists());
    }

    #[test]
    fn links_are_never_followed_and_foreign_caches_are_skipped() {
        let fixture = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        let debug = fixture.profile('a', "debug", 4096, 100);
        std::os::unix::fs::symlink(outside.path(), debug.join("deps/escape")).unwrap();
        // A symlinked profile directory and a cache with loose permissions.
        let target = fixture.cache('a').join("target");
        std::os::unix::fs::symlink(outside.path(), target.join("linked")).unwrap();
        let loose = fixture.profile('b', "debug", 4096, 100);
        fs::set_permissions(fixture.cache('b'), fs::Permissions::from_mode(0o755)).unwrap();
        let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
        assert_eq!(report.removed, vec![debug.clone()]);
        assert!(outside.path().join("keep").exists());
        assert!(loose.exists());
    }

    #[test]
    fn read_only_directories_are_removed() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 100);
        fs::set_permissions(debug.join("deps"), fs::Permissions::from_mode(0o500)).unwrap();
        let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
        assert_eq!(report.removed, vec![debug.clone()]);
        assert!(!debug.exists());
    }

    #[test]
    fn collection_refuses_while_the_admission_lock_is_held() {
        provenant_sqlite::with_busy_timeout(std::time::Duration::ZERO, || {
            let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
            fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let fixture = Fixture::new();
            let debug = fixture.profile('a', "debug", 4096, 100);
            let clock = crate::tests::clock();
            let cancel = provenant_core::CancelToken::new();
            let request = crate::Request {
                clock: &clock,
                cancel: &cancel,
            };
            // A claim in flight holds the ledger: collection refuses and deletes nothing.
            let outcome = crate::ledger::while_admission_held(state.path(), &request, || {
                collect(state.path(), fixture.home.path(), limits(0, 0), false).is_err()
            })
            .unwrap();
            assert!(outcome);
            assert!(debug.exists());
            let report = collect(state.path(), fixture.home.path(), limits(0, 0), false).unwrap();
            assert_eq!(report.removed, vec![debug.clone()]);
        })
    }

    #[test]
    fn a_directory_written_within_the_grace_is_kept_though_its_locks_are_free() {
        let fixture = Fixture::new();
        let running = fixture.profile('a', "debug", 4096, 60);
        let settled = fixture.profile('b', "debug", 4096, 1000);
        // A test binary still runs after Cargo dropped its locks.
        let report = sweep(
            fixture.home.path(),
            graced(0, 900),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![settled.clone()]);
        assert_eq!(report.recent, vec![running.clone()]);
        assert!(report.busy.is_empty());
        assert!(running.join("deps/payload").exists() && !settled.exists());
        assert!(report.cache_bytes >= 4096, "kept bytes still count");
        // A dry run predicts the same, and a shorter grace releases it.
        let preview = sweep(
            fixture.home.path(),
            graced(0, 900),
            true,
            now(),
            &mut plenty,
        );
        assert!(preview.removed.is_empty() && preview.recent == vec![running.clone()]);
        let report = sweep(
            fixture.home.path(),
            graced(0, 30),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![running.clone()]);
    }

    #[test]
    fn a_write_after_the_walk_keeps_the_directory() {
        // Cargo opens `.cargo-lock` or creates a directory after the walk
        // measured the profile and before the sweep takes the flocks.
        for touch in ["lock", "directory", "payload"] {
            let fixture = Fixture::new();
            let debug = fixture.profile('a', "debug", 4096, 1000);
            let mut free = plenty;
            let survey = survey(fixture.home.path(), &mut free, false);
            let when = fixed_time();
            match touch {
                "lock" => fs::File::open(debug.join(CARGO_LOCK))
                    .unwrap()
                    .set_modified(when)
                    .unwrap(),
                "directory" => fs::create_dir_all(debug.join("deps/incremental")).unwrap(),
                _ => fs::write(debug.join("deps/payload"), b"more").unwrap(),
            }
            let report = prune(
                survey,
                graced(0, 900),
                false,
                now(),
                &mut free,
                &TargetProtection::default(),
            );
            assert!(report.removed.is_empty(), "{touch}");
            assert_eq!(report.recent, vec![debug.clone()], "{touch}");
            assert!(debug.join("deps/payload").exists(), "{touch}");
        }
    }

    #[test]
    fn a_change_since_the_walk_is_noticed_even_without_a_grace() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 1000);
        let mut free = plenty;
        let survey = survey(fixture.home.path(), &mut free, false);
        // Newer than the walk saw, yet far older than any grace.
        let later = fixed_time() - std::time::Duration::from_secs(500);
        fs::File::open(debug.join("deps/payload"))
            .unwrap()
            .set_modified(later)
            .unwrap();
        let report = prune(
            survey,
            limits(0, 0),
            false,
            now(),
            &mut free,
            &TargetProtection::default(),
        );
        assert!(report.removed.is_empty() && debug.exists());
        assert_eq!(report.recent, vec![debug.clone()]);
    }

    #[test]
    fn busy_directories_count_toward_the_cap_and_idle_ones_all_go_when_it_is_exceeded() {
        let fixture = Fixture::new();
        let busy_a = fixture.profile('a', "debug", 8192, 3000);
        let busy_b = fixture.profile('b', "debug", 8192, 2000);
        let idle_a = fixture.profile('c', "debug", 8192, 1000);
        let idle_b = fixture.profile('d', "debug", 8192, 900);
        let _a = hold(&busy_a, CARGO_LOCK);
        let _b = hold(&busy_b, LOCKS[1]);
        let mut free = plenty;
        let survey = survey(fixture.home.path(), &mut free, false);
        let idle_bytes: u64 = survey
            .candidates
            .iter()
            .filter(|candidate| candidate.display == idle_a || candidate.display == idle_b)
            .map(|candidate| candidate.bytes)
            .sum();
        // Held targets alone exceed the cap, so every idle neighbour is reclaimed.
        let cap = (survey.total - idle_bytes).saturating_sub(1);
        let report = sweep(
            fixture.home.path(),
            limits(cap, 0),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![idle_a.clone(), idle_b.clone()]);
        assert_eq!(report.busy, vec![busy_a.clone(), busy_b.clone()]);
        assert!(busy_a.exists() && busy_b.exists());
        assert!(report.cache_bytes > cap, "stays over the cap");
    }

    #[test]
    fn a_sweep_with_nothing_to_do_does_not_take_the_admission_lock() {
        let state = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 1000);
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let request = crate::Request {
            clock: &clock,
            cancel: &cancel,
        };
        // A claim in flight holds the ledger; a sweep that has nothing to remove
        // neither waits on it nor fails, and still measures the caches.
        let report = crate::ledger::while_admission_held(state.path(), &request, || {
            collect(
                state.path(),
                fixture.home.path(),
                graced(u64::MAX, 0),
                false,
            )
        })
        .unwrap()
        .unwrap();
        assert!(report.removed.is_empty() && debug.exists());
        assert!(report.cache_bytes >= 4096);
    }

    #[test]
    fn a_future_modification_beyond_the_skew_is_not_recent() {
        let grace = 900 * 1_000_000_000_i128;
        let now = 10_000 * 1_000_000_000_i128;
        let sec = 1_000_000_000_i128;
        assert!(within_grace(now - 60 * sec, now, grace));
        assert!(!within_grace(now - 1000 * sec, now, grace));
        // A little skew is tolerated; a clock set days ahead is not trusted.
        assert!(within_grace(now + 60 * sec, now, grace));
        assert!(!within_grace(now + 6 * 60 * sec, now, grace));
        assert!(!within_grace(now + 86_400 * sec, now, grace));
    }

    #[test]
    fn sizes_are_allocated_bytes_not_apparent_size() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 1, 100);
        // A sparse file is large by length and nearly empty on disk.
        let sparse = fs::File::create(debug.join("deps/sparse")).unwrap();
        sparse.set_len(1 << 30).unwrap();
        let report = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            true,
            now(),
            &mut plenty,
        );
        assert!(report.cache_bytes < (1 << 24), "{}", report.cache_bytes);
    }
}
