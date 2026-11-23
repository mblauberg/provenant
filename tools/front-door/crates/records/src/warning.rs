//! The one way library code reports a warning: a typed value with a stable
//! code, routed to a capture, the command's sink or stderr.
use crate::provenance::Source;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::{Arc, RwLock};

/// A typed warning. The code is stable (like a refusal code, ADR 0034); the
/// message is one sentence for a person and never carries a secret value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    /// Stable UPPER_SNAKE code from the declared catalogue in this module.
    pub code: Cow<'static, str>,
    /// One sentence for a person; never a secret value (redaction as for errors).
    pub message: String,
    /// What it is about: a path, key, branch, attempt or route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The setting that caused it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// One command or edit that clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    /// How repeats collapse. Never part of the wire.
    #[serde(skip)]
    pub dedup: Dedup,
}

/// How a repeated warning collapses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dedup {
    /// The same key collapses within one command (count kept).
    #[default]
    PerInvocation,
    /// Shown once until its key changes; remembered in the state root.
    PerChange,
}

/// A declared warning code: one const per code with a one-line meaning.
/// Adding a code is additive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WarningCode(pub &'static str);

impl WarningCode {
    /// The stable UPPER_SNAKE spelling.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// A configured key was ignored: it is not part of the declared schema.
pub const CONFIG_KEY_IGNORED: WarningCode = WarningCode("CONFIG_KEY_IGNORED");
/// A configured value was malformed, so the default applied.
pub const CONFIG_MALFORMED: WarningCode = WarningCode("CONFIG_MALFORMED");
/// A candidate value came from a layer its family does not list.
pub const SETTING_LAYER_IGNORED: WarningCode = WarningCode("SETTING_LAYER_IGNORED");
/// A name differed only by case and was folded to its canonical spelling.
pub const NAME_CASE_FOLDED: WarningCode = WarningCode("NAME_CASE_FOLDED");
/// A name matched a declared alias rather than its canonical spelling.
pub const NAME_ALIAS_USED: WarningCode = WarningCode("NAME_ALIAS_USED");
/// The lane's branch is behind its base and will be updated.
pub const BRANCH_BEHIND_BASE: WarningCode = WarningCode("BRANCH_BEHIND_BASE");
/// An existing worktree was adopted although it had uncommitted changes.
pub const WORKTREE_ADOPTED_DIRTY: WarningCode = WarningCode("WORKTREE_ADOPTED_DIRTY");
/// This release reads and writes a different live-record format.
pub const LIVE_RECORD_FORMAT_CHANGED: WarningCode = WarningCode("LIVE_RECORD_FORMAT_CHANGED");
/// One install-time migration step failed; the install continues.
pub const MIGRATION_STEP_FAILED: WarningCode = WarningCode("MIGRATION_STEP_FAILED");
/// The state root overlaps a location the product treats as advisory.
pub const STATE_ROOT_OVERLAP: WarningCode = WarningCode("STATE_ROOT_OVERLAP");
/// The one forge context could not be resolved, so a fail-closed path applies.
pub const FORGE_CONTEXT_UNRESOLVED: WarningCode = WarningCode("FORGE_CONTEXT_UNRESOLVED");
/// A result exceeded the output cap and was bounded with a marker.
pub const RESULT_TOO_LARGE: WarningCode = WarningCode("RESULT_TOO_LARGE");
/// A lane's unfinished Git operation is retained while its committed tip settles.
pub const GIT_OPERATION_RETAINED: WarningCode = WarningCode("GIT_OPERATION_RETAINED");
/// An invalid stored answer was skipped during local delivery.
pub const ANSWER_DELIVERY_SKIPPED: WarningCode = WarningCode("ANSWER_DELIVERY_SKIPPED");
/// An oversized stored answer was retained for Fabric mail during local delivery.
pub const ANSWER_DELIVERY_RETAINED: WarningCode = WarningCode("ANSWER_DELIVERY_RETAINED");
/// A holder's terminal failure could not be retained in the settlement journal.
pub const TERMINAL_FAILURE_UNRETAINED: WarningCode = WarningCode("TERMINAL_FAILURE_UNRETAINED");
/// Terminal settlement was incomplete, so the writer reservation is retained.
pub const TERMINAL_PUBLICATION_INCOMPLETE: WarningCode =
    WarningCode("TERMINAL_PUBLICATION_INCOMPLETE");
/// A non-boundary policy rule was violated; the caller warns and proceeds.
pub const POLICY_WARNING: WarningCode = WarningCode("POLICY_WARNING");
/// A configuration source could not be read, so compiled defaults applied.
pub const CONFIG_UNAVAILABLE: WarningCode = WarningCode("CONFIG_UNAVAILABLE");
/// An unrecognised entry in the project registry was ignored.
pub const PROJECT_DIRECTORY_IGNORED: WarningCode = WarningCode("PROJECT_DIRECTORY_IGNORED");
/// Free disk space fell below the configured floor; collection may proceed.
pub const DISK_FLOOR_LOW: WarningCode = WarningCode("DISK_FLOOR_LOW");
/// Native package cache settings could not be resolved safely and were kept.
pub const PACKAGE_CACHE_SETTINGS_UNRESOLVED: WarningCode =
    WarningCode("PACKAGE_CACHE_SETTINGS_UNRESOLVED");
/// Project pnpm cache settings use a legacy spelling that versions read differently.
pub const LEGACY_PNPM_CACHE_SETTINGS: WarningCode = WarningCode("LEGACY_PNPM_CACHE_SETTINGS");
/// The effective pnpm cache selector family could not be identified safely.
pub const PNPM_FAMILY_UNRESOLVED: WarningCode = WarningCode("PNPM_FAMILY_UNRESOLVED");
/// The configured pnpm cache selectors disagree with the selected version.
pub const PNPM_SELECTOR_CONFLICT: WarningCode = WarningCode("PNPM_SELECTOR_CONFLICT");
/// Pre-11 pnpm cache binding uses environment selectors npm may report as unknown.
pub const LEGACY_PNPM_ENV_SELECTOR: WarningCode = WarningCode("LEGACY_PNPM_ENV_SELECTOR");
/// Reconciling a live quota provider's account switch failed.
pub const ACCOUNT_SWITCH_RECONCILE_FAILED: WarningCode =
    WarningCode("ACCOUNT_SWITCH_RECONCILE_FAILED");
/// A post-commit change signal could not be published.
pub const CHANGE_SIGNAL_UNPUBLISHED: WarningCode = WarningCode("CHANGE_SIGNAL_UNPUBLISHED");
/// A detached worker's process-budget probe failed; the limit is unknown.
pub const PROCESS_BUDGET_PROBE_FAILED: WarningCode = WarningCode("PROCESS_BUDGET_PROBE_FAILED");

// Warnings from the remaining library crates (w2-warn-lib-b): full meaning and
// the sites that use each code are recorded beside the conversion in that
// slice. Kept in one block so the catalogue stays easy to audit.
/// The credential symlink inventory could not be fully inspected.
pub const CREDENTIAL_INVENTORY_INCOMPLETE: WarningCode =
    WarningCode("CREDENTIAL_INVENTORY_INCOMPLETE");
/// A host inventory entry could not be inspected; that path is protected.
pub const HOST_INVENTORY_INCOMPLETE: WarningCode = WarningCode("HOST_INVENTORY_INCOMPLETE");
/// A process could not be inspected (EPERM); its custody is unknown.
pub const UNREADABLE_PROCESS_CENSUS: WarningCode = WarningCode("UNREADABLE_PROCESS_CENSUS");
/// The origin default branch was unavailable, so local HEAD was used.
pub const DEFAULT_BASE_FALLBACK: WarningCode = WarningCode("DEFAULT_BASE_FALLBACK");
/// A post-create check failed; the worktree remains usable at its path.
pub const WORKTREE_SETUP_INCOMPLETE: WarningCode = WarningCode("WORKTREE_SETUP_INCOMPLETE");
/// A worktree lane cache could not be removed.
pub const CACHE_REMOVAL_FAILED: WarningCode = WarningCode("CACHE_REMOVAL_FAILED");
/// A pre-push rewrite of the default branch was allowed by policy.
pub const DEFAULT_BRANCH_REWRITTEN: WarningCode = WarningCode("DEFAULT_BRANCH_REWRITTEN");
/// Conflict markers in pushed commits were allowed by policy.
pub const CONFLICT_MARKERS_PUSHED: WarningCode = WarningCode("CONFLICT_MARKERS_PUSHED");
/// Advisory: custody of a fabric subject changed and was recorded.
pub const CUSTODY_CHANGED: WarningCode = WarningCode("CUSTODY_CHANGED");
/// Advisory: a fabric subject is held by another actor.
pub const CUSTODY_HELD_ADVISORY: WarningCode = WarningCode("CUSTODY_HELD_ADVISORY");
/// A field matched a credential shape; matched bytes are never reported.
pub const SECRET_SHAPE_IN_FIELD: WarningCode = WarningCode("SECRET_SHAPE_IN_FIELD");
/// Graph observation indexing was deferred; the checkpoint is still stored.
pub const GRAPH_OBSERVATION_DEFERRED: WarningCode = WarningCode("GRAPH_OBSERVATION_DEFERRED");
/// A resource probe failed; the shipped policy value continues to apply.
pub const RESOURCE_PROBE_FAILED: WarningCode = WarningCode("RESOURCE_PROBE_FAILED");
/// Available memory fell below the configured floor.
pub const MEMORY_FLOOR_LOW: WarningCode = WarningCode("MEMORY_FLOOR_LOW");
/// A build slot was unavailable; the job built locally instead.
pub const BUILD_SLOT_UNAVAILABLE: WarningCode = WarningCode("BUILD_SLOT_UNAVAILABLE");
/// A lane-port block was unavailable for a build job.
pub const BUILD_PORT_UNAVAILABLE: WarningCode = WarningCode("BUILD_PORT_UNAVAILABLE");
/// A build ledger diagnostic that does not change admission.
pub const BUILD_LEDGER_DIAGNOSTIC: WarningCode = WarningCode("BUILD_LEDGER_DIAGNOSTIC");
/// A delivery transport advisory for a host-configured Git transport.
pub const DELIVERY_TRANSPORT_ADVISORY: WarningCode = WarningCode("DELIVERY_TRANSPORT_ADVISORY");
/// A private Git directory was retained after a refused import.
pub const PRIVATE_GIT_RETAINED: WarningCode = WarningCode("PRIVATE_GIT_RETAINED");
/// The primary checkout could not be checked for stray lane work.
pub const STRAY_WORK_CHECK_FAILED: WarningCode = WarningCode("STRAY_WORK_CHECK_FAILED");
/// Stray lane work was observed in the primary checkout.
pub const STRAY_LANE_WORK: WarningCode = WarningCode("STRAY_LANE_WORK");
/// The sandbox was unavailable; the environment fallback was used.
pub const SANDBOX_UNAVAILABLE: WarningCode = WarningCode("SANDBOX_UNAVAILABLE");
/// Lane priority was unavailable; the compiled default was used.
pub const LANE_PRIORITY_FALLBACK: WarningCode = WarningCode("LANE_PRIORITY_FALLBACK");
/// Run settings were unavailable; a shipped default was used.
pub const RUN_SETTINGS_UNAVAILABLE: WarningCode = WarningCode("RUN_SETTINGS_UNAVAILABLE");
/// A PR contribution could not be fully attributed.
pub const PR_CONTRIBUTION_WARNING: WarningCode = WarningCode("PR_CONTRIBUTION_WARNING");
/// A settlement diagnostic that does not change the terminal outcome.
pub const SETTLEMENT_DIAGNOSTIC: WarningCode = WarningCode("SETTLEMENT_DIAGNOSTIC");
/// A policy advisory; the policy rule is carried in the subject.
pub const POLICY_ADVISORY: WarningCode = WarningCode("POLICY_ADVISORY");
/// A tool-state diagnostic retained as settlement bookkeeping.
pub const TOOL_STATE_DIAGNOSTIC: WarningCode = WarningCode("TOOL_STATE_DIAGNOSTIC");
/// An admission receipt is visible but its durability is unconfirmed.
pub const ADMISSION_RECEIPT_UNCONFIRMED: WarningCode = WarningCode("ADMISSION_RECEIPT_UNCONFIRMED");
/// Repository admission release is pending or failed; reconcile retries it.
pub const ADMISSION_RELEASE_PENDING: WarningCode = WarningCode("ADMISSION_RELEASE_PENDING");
/// Repository admission skipped an attempt and left it queued.
pub const REPOSITORY_ADMISSION_SKIPPED: WarningCode = WarningCode("REPOSITORY_ADMISSION_SKIPPED");
/// A run-store path had its permissions tightened to a private mode.
pub const RUN_STORE_MODE_TIGHTENED: WarningCode = WarningCode("RUN_STORE_MODE_TIGHTENED");
/// A lane's Git history is not rooted where publication expected.
pub const LANE_HISTORY_WARNING: WarningCode = WarningCode("LANE_HISTORY_WARNING");
/// A lane Git import reported bounded advice on stderr.
pub const LANE_GIT_IMPORT_ADVICE: WarningCode = WarningCode("LANE_GIT_IMPORT_ADVICE");
/// Native index synchronisation was advisory and did not fail settlement.
pub const NATIVE_INDEX_ADVISORY: WarningCode = WarningCode("NATIVE_INDEX_ADVISORY");
/// A lane handoff was accepted with an advisory parse notice.
pub const HANDOFF_ADVISORY: WarningCode = WarningCode("HANDOFF_ADVISORY");
/// A build slot custody publication failed before spawn.
pub const BUILD_SLOT_SETUP_WARNING: WarningCode = WarningCode("BUILD_SLOT_SETUP_WARNING");
/// A build directory GC diagnostic preserved its targets.
pub const BUILD_DIR_GC_ADVISORY: WarningCode = WarningCode("BUILD_DIR_GC_ADVISORY");
/// A delivery Git refresh advisory left the lane usable.
pub const DELIVERY_REFRESH_ADVISORY: WarningCode = WarningCode("DELIVERY_REFRESH_ADVISORY");
/// A base fetch advisory left the launch to continue or retry.
pub const BASE_FETCH_ADVISORY: WarningCode = WarningCode("BASE_FETCH_ADVISORY");
/// Stop publication was unavailable; custody was retained.
pub const STOP_PUBLICATION_UNAVAILABLE: WarningCode = WarningCode("STOP_PUBLICATION_UNAVAILABLE");
/// Stopped work remains unpushed in the primary checkout.
pub const WRITER_UNPUSHED_WORK: WarningCode = WarningCode("WRITER_UNPUSHED_WORK");
/// The primary checkout position could not be recorded.
pub const PRIMARY_POSITION_UNRECORDED: WarningCode = WarningCode("PRIMARY_POSITION_UNRECORDED");
/// A stored admission lease row was malformed and skipped.
pub const ADMISSION_RECORD_MALFORMED: WarningCode = WarningCode("ADMISSION_RECORD_MALFORMED");

impl Warning {
    /// A warning with no subject, source or repair.
    pub fn new(code: WarningCode, message: impl Into<String>) -> Self {
        Self {
            code: Cow::Borrowed(code.0),
            message: message.into(),
            subject: None,
            source: None,
            repair: None,
            dedup: Dedup::default(),
        }
    }

    /// Name what the warning is about.
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Name the setting that caused it.
    pub fn source(mut self, source: Source) -> Self {
        self.source = Some(source);
        self
    }

    /// State one command or edit that clears it.
    pub fn repair(mut self, repair: impl Into<String>) -> Self {
        self.repair = Some(repair.into());
        self
    }

    /// Show it once until its key changes instead of once per invocation.
    pub fn once_per_change(mut self) -> Self {
        self.dedup = Dedup::PerChange;
        self
    }

    /// sha256 over code, subject and message: the dedup key.
    pub fn key(&self) -> String {
        use sha2::{Digest as _, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"provenant-warning\0");
        digest.update(self.code.as_bytes());
        digest.update(b"\0");
        digest.update(self.subject.as_deref().unwrap_or_default().as_bytes());
        digest.update(b"\0");
        digest.update(self.message.as_bytes());
        format!("sha256:{}", provenant_core::hex(&digest.finalize()))
    }
}

/// A command's warning consumer. The binary edge installs it.
pub trait Sink: Send + Sync {
    /// Accept one warning.
    fn accept(&self, w: Warning);
}

/// Installs the process sink for the current command; dropping the guard
/// restores the previous one.
pub fn install(sink: Arc<dyn Sink>) -> SinkGuard {
    let previous = SINK
        .write()
        .map(|mut slot| slot.replace(sink))
        .unwrap_or(None);
    SinkGuard { previous }
}

/// Restores the process sink that was installed before its [`install`].
pub struct SinkGuard {
    previous: Option<Arc<dyn Sink>>,
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = SINK.write() {
            *slot = self.previous.take();
        }
    }
}

/// The only way library code reports a warning. Routes to, in order: the
/// innermost capture on this thread; the process sink the binary installed
/// for this command (one command per process, so worker threads reach it);
/// else stderr as `warning: <message>`.
#[allow(clippy::print_stderr, clippy::disallowed_methods)]
pub fn warn(w: Warning) {
    let captured = CAPTURES.with(|captures| {
        let mut stack = captures.borrow_mut();
        match stack.last_mut() {
            Some(frame) => {
                frame.push(w.clone());
                true
            }
            None => false,
        }
    });
    if captured {
        return;
    }
    let sink = SINK.read().map(|slot| slot.clone()).unwrap_or(None);
    if let Some(sink) = sink {
        sink.accept(w);
        return;
    }
    eprintln!("warning: {}", w.message);
}

/// Thread-local: collects warnings raised on this thread inside `f` (tests, or
/// an owner that must return them as data). Parallel tests do not see each
/// other's warnings.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Warning>) {
    struct PopOnDrop;
    impl Drop for PopOnDrop {
        fn drop(&mut self) {
            CAPTURES.with(|captures| {
                captures.borrow_mut().pop();
            });
        }
    }

    CAPTURES.with(|captures| captures.borrow_mut().push(Vec::new()));
    let frame = PopOnDrop;
    let result = f();
    let warnings = CAPTURES.with(|captures| {
        captures
            .borrow_mut()
            .last_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    });
    drop(frame);
    (result, warnings)
}

thread_local! {
    static CAPTURES: RefCell<Vec<Vec<Warning>>> = const { RefCell::new(Vec::new()) };
}

static SINK: RwLock<Option<Arc<dyn Sink>>> = RwLock::new(None);

#[cfg(test)]
mod tests {
    use super::{Dedup, NAME_CASE_FOLDED, Sink, Warning, capture, install, warn};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Recorder {
        messages: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn messages(&self) -> Vec<String> {
            self.messages.lock().expect("recorder lock").clone()
        }
    }

    impl Sink for Recorder {
        fn accept(&self, warning: Warning) {
            self.messages
                .lock()
                .expect("recorder lock")
                .push(warning.message);
        }
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn capture_collects_warnings_and_prints_nothing() {
        let ((), warnings) = capture(|| {
            warn(Warning::new(
                NAME_CASE_FOLDED,
                "folded `Writer` to `writer`",
            ));
            warn(Warning::new(NAME_CASE_FOLDED, "folded `Reader` to `reader`").subject("reader"));
        });
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].code.as_ref(), NAME_CASE_FOLDED.0);
        assert_eq!(warnings[1].subject.as_deref(), Some("reader"));
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn nested_sinks_restore_the_previous_one() {
        let outer = Arc::new(Recorder::default());
        let inner = Arc::new(Recorder::default());
        let outer_guard = install(outer.clone());
        {
            let _inner_guard = install(inner.clone());
            warn(Warning::new(NAME_CASE_FOLDED, "inner"));
        }
        warn(Warning::new(NAME_CASE_FOLDED, "outer"));
        assert_eq!(inner.messages(), ["inner"]);
        assert_eq!(outer.messages(), ["outer"]);
        drop(outer_guard);
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn a_per_change_key_is_stable_for_equal_content_and_follows_the_subject() {
        let first = Warning::new(NAME_CASE_FOLDED, "folded")
            .subject("writer")
            .once_per_change();
        let same = Warning::new(NAME_CASE_FOLDED, "folded")
            .subject("writer")
            .once_per_change();
        let other = Warning::new(NAME_CASE_FOLDED, "folded")
            .subject("reader")
            .once_per_change();
        assert_eq!(first.dedup, Dedup::PerChange);
        assert_eq!(first.key(), same.key());
        assert_ne!(first.key(), other.key());
        assert!(first.key().starts_with("sha256:"));
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn the_library_crate_codes_are_upper_snake() {
        for code in [
            super::CREDENTIAL_INVENTORY_INCOMPLETE,
            super::HOST_INVENTORY_INCOMPLETE,
            super::UNREADABLE_PROCESS_CENSUS,
            super::DEFAULT_BASE_FALLBACK,
            super::WORKTREE_SETUP_INCOMPLETE,
            super::CACHE_REMOVAL_FAILED,
            super::DEFAULT_BRANCH_REWRITTEN,
            super::CONFLICT_MARKERS_PUSHED,
            super::CUSTODY_CHANGED,
            super::CUSTODY_HELD_ADVISORY,
            super::SECRET_SHAPE_IN_FIELD,
            super::GRAPH_OBSERVATION_DEFERRED,
            super::RESOURCE_PROBE_FAILED,
            super::MEMORY_FLOOR_LOW,
            super::BUILD_SLOT_UNAVAILABLE,
            super::BUILD_PORT_UNAVAILABLE,
            super::BUILD_LEDGER_DIAGNOSTIC,
            super::DELIVERY_TRANSPORT_ADVISORY,
            super::PRIVATE_GIT_RETAINED,
            super::STRAY_WORK_CHECK_FAILED,
            super::STRAY_LANE_WORK,
            super::SANDBOX_UNAVAILABLE,
            super::LANE_PRIORITY_FALLBACK,
            super::RUN_SETTINGS_UNAVAILABLE,
            super::PR_CONTRIBUTION_WARNING,
            super::SETTLEMENT_DIAGNOSTIC,
            super::POLICY_ADVISORY,
            super::TOOL_STATE_DIAGNOSTIC,
            super::ADMISSION_RECEIPT_UNCONFIRMED,
            super::ADMISSION_RELEASE_PENDING,
            super::REPOSITORY_ADMISSION_SKIPPED,
            super::RUN_STORE_MODE_TIGHTENED,
            super::LANE_HISTORY_WARNING,
            super::LANE_GIT_IMPORT_ADVICE,
            super::NATIVE_INDEX_ADVISORY,
            super::HANDOFF_ADVISORY,
            super::BUILD_SLOT_SETUP_WARNING,
            super::BUILD_DIR_GC_ADVISORY,
            super::DELIVERY_REFRESH_ADVISORY,
            super::BASE_FETCH_ADVISORY,
            super::STOP_PUBLICATION_UNAVAILABLE,
            super::WRITER_UNPUSHED_WORK,
            super::PRIMARY_POSITION_UNRECORDED,
            super::ADMISSION_RECORD_MALFORMED,
        ] {
            let name = code.as_str();
            assert!(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte == b'_'),
                "{name}"
            );
        }
    }

    #[test]
    fn the_catalogue_codes_are_upper_snake() {
        for code in [
            super::CONFIG_KEY_IGNORED,
            super::CONFIG_MALFORMED,
            super::SETTING_LAYER_IGNORED,
            super::NAME_CASE_FOLDED,
            super::NAME_ALIAS_USED,
            super::BRANCH_BEHIND_BASE,
            super::WORKTREE_ADOPTED_DIRTY,
            super::LIVE_RECORD_FORMAT_CHANGED,
            super::MIGRATION_STEP_FAILED,
            super::STATE_ROOT_OVERLAP,
            super::RESULT_TOO_LARGE,
            super::FORGE_CONTEXT_UNRESOLVED,
            super::GIT_OPERATION_RETAINED,
            super::ANSWER_DELIVERY_SKIPPED,
            super::ANSWER_DELIVERY_RETAINED,
            super::TERMINAL_FAILURE_UNRETAINED,
            super::TERMINAL_PUBLICATION_INCOMPLETE,
            super::POLICY_WARNING,
            super::CONFIG_UNAVAILABLE,
            super::PROJECT_DIRECTORY_IGNORED,
            super::DISK_FLOOR_LOW,
            super::PACKAGE_CACHE_SETTINGS_UNRESOLVED,
            super::LEGACY_PNPM_CACHE_SETTINGS,
            super::PNPM_FAMILY_UNRESOLVED,
            super::PNPM_SELECTOR_CONFLICT,
            super::LEGACY_PNPM_ENV_SELECTOR,
            super::ACCOUNT_SWITCH_RECONCILE_FAILED,
            super::CHANGE_SIGNAL_UNPUBLISHED,
            super::PROCESS_BUDGET_PROBE_FAILED,
        ] {
            let name = code.as_str();
            assert!(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte == b'_'),
                "{name}"
            );
        }
    }
}
