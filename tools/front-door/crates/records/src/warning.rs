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
/// A live provider emitted no output past its configured stall threshold.
/// Reporting is read-only; an opt-in policy may act after this warning.
pub const PROVIDER_STALLED: WarningCode = WarningCode("PROVIDER_STALLED");
/// An invalid stored answer was skipped during local delivery.
pub const ANSWER_DELIVERY_SKIPPED: WarningCode = WarningCode("ANSWER_DELIVERY_SKIPPED");
/// An oversized stored answer was retained for Fabric mail during local delivery.
pub const ANSWER_DELIVERY_RETAINED: WarningCode = WarningCode("ANSWER_DELIVERY_RETAINED");
/// A holder's terminal failure could not be retained in the settlement journal.
pub const TERMINAL_FAILURE_UNRETAINED: WarningCode = WarningCode("TERMINAL_FAILURE_UNRETAINED");
/// Terminal settlement was incomplete, so the writer reservation is retained.
pub const TERMINAL_PUBLICATION_INCOMPLETE: WarningCode =
    WarningCode("TERMINAL_PUBLICATION_INCOMPLETE");
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
/// An unreadable process was judged non-occupying and did not block a census.
pub const UNREADABLE_PROCESS_NON_OCCUPYING: WarningCode =
    WarningCode("UNREADABLE_PROCESS_NON_OCCUPYING");
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
/// A fabric subject is held by another actor; the observation is advisory.
pub const CUSTODY_HELD: WarningCode = WarningCode("CUSTODY_HELD");
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
/// A build ledger note was raised for the operator.
pub const BUILD_LEDGER_NOTE: WarningCode = WarningCode("BUILD_LEDGER_NOTE");
/// A host-configured Git transport was unavailable; a fallback applied.
pub const DELIVERY_TRANSPORT_FALLBACK: WarningCode = WarningCode("DELIVERY_TRANSPORT_FALLBACK");
/// A private Git directory was retained after a refused import.
pub const PRIVATE_GIT_RETAINED: WarningCode = WarningCode("PRIVATE_GIT_RETAINED");
/// The primary checkout could not be checked for stray lane work.
pub const STRAY_WORK_CHECK_FAILED: WarningCode = WarningCode("STRAY_WORK_CHECK_FAILED");
/// Stray lane work was observed in the primary checkout.
pub const STRAY_LANE_WORK: WarningCode = WarningCode("STRAY_LANE_WORK");
/// The primary checkout moved while the bound branch advanced; the move is not
/// attributed to the lane and is left intact.
pub const PRIMARY_MOVED_UNATTRIBUTED: WarningCode = WarningCode("PRIMARY_MOVED_UNATTRIBUTED");
/// The sandbox was unavailable; the environment fallback was used.
pub const SANDBOX_UNAVAILABLE: WarningCode = WarningCode("SANDBOX_UNAVAILABLE");
/// Lane priority was unavailable; the compiled default was used.
pub const LANE_PRIORITY_FALLBACK: WarningCode = WarningCode("LANE_PRIORITY_FALLBACK");
/// Run settings were unavailable; a shipped default was used.
pub const RUN_SETTINGS_UNAVAILABLE: WarningCode = WarningCode("RUN_SETTINGS_UNAVAILABLE");
/// A PR contribution could not be bound to its author.
pub const PR_CONTRIBUTION_BINDING_FAILED: WarningCode =
    WarningCode("PR_CONTRIBUTION_BINDING_FAILED");
/// Writer fence listing failed; the settlement outcome is unchanged.
pub const WRITER_FENCE_LIST_FAILED: WarningCode = WarningCode("WRITER_FENCE_LIST_FAILED");
/// A settled writer attempt named a retained fence that is missing; abandon proceeds.
pub const ABANDON_RETAINED_FENCE_MISSING: WarningCode =
    WarningCode("ABANDON_RETAINED_FENCE_MISSING");
/// A dead lane's attributed process was found but cannot be proven enough to
/// reap; it is named in status for the chair.
pub const ORPHAN_PROCESS_ATTRIBUTED: WarningCode = WarningCode("ORPHAN_PROCESS_ATTRIBUTED");
/// An abandoned lane left a stale Git worktree registration; the lane settled.
pub const ABANDON_STALE_WORKTREE_REGISTRATION: WarningCode =
    WarningCode("ABANDON_STALE_WORKTREE_REGISTRATION");
/// A lane tool-state commit was refused by policy; settlement continues.
pub const TOOL_STATE_COMMIT_FAILED: WarningCode = WarningCode("TOOL_STATE_COMMIT_FAILED");
/// An admission receipt is visible but its durability is unconfirmed.
pub const ADMISSION_RECEIPT_UNCONFIRMED: WarningCode = WarningCode("ADMISSION_RECEIPT_UNCONFIRMED");
/// Repository admission release is pending or failed; reconcile retries it.
pub const ADMISSION_RELEASE_PENDING: WarningCode = WarningCode("ADMISSION_RELEASE_PENDING");
/// Repository admission skipped an attempt and left it queued.
pub const REPOSITORY_ADMISSION_SKIPPED: WarningCode = WarningCode("REPOSITORY_ADMISSION_SKIPPED");
/// A run-store path had its permissions tightened to a private mode.
pub const RUN_STORE_MODE_TIGHTENED: WarningCode = WarningCode("RUN_STORE_MODE_TIGHTENED");
/// A lane's Git history needed a publication adjustment.
pub const LANE_HISTORY_PUBLICATION_ADJUSTED: WarningCode =
    WarningCode("LANE_HISTORY_PUBLICATION_ADJUSTED");
/// A lane Git import reported bounded advice on stderr.
pub const LANE_GIT_IMPORT_STDERR: WarningCode = WarningCode("LANE_GIT_IMPORT_STDERR");
/// Native index synchronisation failed; settlement proceeds.
pub const NATIVE_INDEX_SYNC_FAILED: WarningCode = WarningCode("NATIVE_INDEX_SYNC_FAILED");
/// A lane handoff was accepted with an advisory parse notice.
pub const HANDOFF_PARSE_NOTICE: WarningCode = WarningCode("HANDOFF_PARSE_NOTICE");
/// A terminal read-only review's reservation was retained without Git work; it no longer fences custody.
pub const READ_ONLY_RESERVATION_RETAINED: WarningCode =
    WarningCode("READ_ONLY_RESERVATION_RETAINED");
/// A build slot custody publication failed before spawn.
pub const BUILD_SLOT_CUSTODY_UNPUBLISHED: WarningCode =
    WarningCode("BUILD_SLOT_CUSTODY_UNPUBLISHED");
/// A build-directory GC pass kept its targets.
pub const BUILD_DIR_GC_TARGET_RETAINED: WarningCode = WarningCode("BUILD_DIR_GC_TARGET_RETAINED");
/// A delivery Git refresh failed; the lane stays usable.
pub const DELIVERY_REFRESH_FAILED: WarningCode = WarningCode("DELIVERY_REFRESH_FAILED");
/// A base fetch hit a lock or transient error; the launch retries or continues.
pub const BASE_FETCH_FALLBACK: WarningCode = WarningCode("BASE_FETCH_FALLBACK");
/// Stop publication was unavailable; custody was retained.
pub const STOP_PUBLICATION_UNAVAILABLE: WarningCode = WarningCode("STOP_PUBLICATION_UNAVAILABLE");
/// Stopped work remains unpushed in the primary checkout.
pub const WRITER_UNPUSHED_WORK: WarningCode = WarningCode("WRITER_UNPUSHED_WORK");
/// The primary checkout position could not be recorded.
pub const PRIMARY_POSITION_UNRECORDED: WarningCode = WarningCode("PRIMARY_POSITION_UNRECORDED");
/// A stored admission lease row was malformed and skipped.
pub const ADMISSION_RECORD_MALFORMED: WarningCode = WarningCode("ADMISSION_RECORD_MALFORMED");
/// A steer targeted a settled lane and was handled by the sourced setting.
pub const STEER_TARGET_SETTLED: WarningCode = WarningCode("STEER_TARGET_SETTLED");

// Event-specific codes added when the warning slices named each site.
// A violated policy rule warns under its own rule code (see policy.rs), so the
// policy effects below name only the call sites that cannot honour a strict
// effect; an advisory below names the one subsystem finding it reports.
/// A strict non-boundary policy refusal was reported while the caller stopped.
pub const POLICY_STRICT_REFUSAL: WarningCode = WarningCode("POLICY_STRICT_REFUSAL");
/// Secret-shaped input policy returned a strict effect the scanner cannot honour.
pub const SECRET_INPUT_POLICY_UNEXPECTED: WarningCode =
    WarningCode("SECRET_INPUT_POLICY_UNEXPECTED");
/// A provider policy rule returned a strict effect launch cannot honour.
pub const PROVIDER_POLICY_UNEXPECTED: WarningCode = WarningCode("PROVIDER_POLICY_UNEXPECTED");
/// A pre-push policy check was advisory and the push continued.
pub const PRE_PUSH_POLICY_ADVISORY: WarningCode = WarningCode("PRE_PUSH_POLICY_ADVISORY");
/// An unsupported pool effort was configured and remains advisory at load.
pub const POOL_EFFORT_UNSUPPORTED: WarningCode = WarningCode("POOL_EFFORT_UNSUPPORTED");
/// A port declaration in an ignored location was skipped.
pub const PORT_DECLARATION_IGNORED: WarningCode = WarningCode("PORT_DECLARATION_IGNORED");
/// An invalid verify port full-after duration fell back to the builtin.
pub const PORT_DURATION_INVALID: WarningCode = WarningCode("PORT_DURATION_INVALID");
/// An invalid `verify.static_checks` entry was dropped from the writer preamble.
pub const PORT_STATIC_CHECK_INVALID: WarningCode = WarningCode("PORT_STATIC_CHECK_INVALID");
/// An installed agent's PATH would fall back from the trusted Rust toolchain.
pub const AGENT_TOOLCHAIN_PATH: WarningCode = WarningCode("AGENT_TOOLCHAIN_PATH");
/// A stale menu login item remains registered after an upgrade.
pub const STALE_LOGIN_ITEM_ADVISORY: WarningCode = WarningCode("STALE_LOGIN_ITEM_ADVISORY");
/// A build priority selector fell back for the effective port.
pub const BUILD_PRIORITY_ADVISORY: WarningCode = WarningCode("BUILD_PRIORITY_ADVISORY");
/// A build-slot GC or prewarm hygiene step retained its targets.
pub const BUILD_HYGIENE_ADVISORY: WarningCode = WarningCode("BUILD_HYGIENE_ADVISORY");
/// A build outcome carried advice for the operator.
pub const BUILD_OUTCOME_ADVISORY: WarningCode = WarningCode("BUILD_OUTCOME_ADVISORY");
/// A session save, checkpoint or delivery carried advice for the operator.
pub const SESSION_ADVISORY: WarningCode = WarningCode("SESSION_ADVISORY");
/// A model is absent from the effective catalogue segment.
pub const MODEL_SEGMENT_ADVISORY: WarningCode = WarningCode("MODEL_SEGMENT_ADVISORY");
/// A fabric launch carried advice for the operator.
pub const LAUNCH_ADVISORY: WarningCode = WarningCode("LAUNCH_ADVISORY");
/// A default creation base fell back to local HEAD.
pub const BASE_FALLBACK_ADVISORY: WarningCode = WarningCode("BASE_FALLBACK_ADVISORY");
/// A graph exclusion was discarded for the accepted route.
pub const EXCLUSION_ADVISORY: WarningCode = WarningCode("EXCLUSION_ADVISORY");
/// An advisory local gate command failed without enforcing the push.
pub const LOCAL_GATE_ADVISORY: WarningCode = WarningCode("LOCAL_GATE_ADVISORY");
/// An install inspection finding was advisory.
pub const INSTALL_FINDING_ADVISORY: WarningCode = WarningCode("INSTALL_FINDING_ADVISORY");
/// A wait budget exceeded its maximum and was clamped.
pub const WAIT_BUDGET_CLAMPED: WarningCode = WarningCode("WAIT_BUDGET_CLAMPED");
/// The model catalogue could not be read; a bundled or empty catalogue applied.
pub const MODEL_CATALOGUE_UNAVAILABLE: WarningCode = WarningCode("MODEL_CATALOGUE_UNAVAILABLE");
/// Quota readiness could not be synchronised with its stored record.
pub const QUOTA_READINESS_UNSYNCED: WarningCode = WarningCode("QUOTA_READINESS_UNSYNCED");
/// A login failure or recovery record could not be published.
pub const LOGIN_RECORD_UNRETAINED: WarningCode = WarningCode("LOGIN_RECORD_UNRETAINED");
/// A configured key uses a deprecated spelling.
pub const CONFIG_DEPRECATION: WarningCode = WarningCode("CONFIG_DEPRECATION");
/// A recorded launch access profile could not be read or applied.
pub const LAUNCH_ACCESS_PROFILE: WarningCode = WarningCode("LAUNCH_ACCESS_PROFILE");
/// A garbage-collection pass could not be scheduled; a later pass retries.
pub const GC_SCHEDULE_FAILED: WarningCode = WarningCode("GC_SCHEDULE_FAILED");
/// Disk space could not be observed; the disk-floor check was skipped.
pub const DISK_OBSERVATION_UNAVAILABLE: WarningCode = WarningCode("DISK_OBSERVATION_UNAVAILABLE");
/// A stored steer restart pacing record was invalid and ignored.
pub const STEER_PACING_RECORD_INVALID: WarningCode = WarningCode("STEER_PACING_RECORD_INVALID");
/// A requested sandbox mode was overridden by the access profile.
pub const SANDBOX_MODE_OVERRIDDEN: WarningCode = WarningCode("SANDBOX_MODE_OVERRIDDEN");
/// A configured upstream or verify base could not be resolved.
pub const UPSTREAM_UNRESOLVED: WarningCode = WarningCode("UPSTREAM_UNRESOLVED");
/// A branch Git configuration value could not be read or parsed.
pub const BRANCH_CONFIG_UNREADABLE: WarningCode = WarningCode("BRANCH_CONFIG_UNREADABLE");
/// A notes cursor acknowledgement failed; a page may replay.
pub const NOTES_CURSOR_UNACKNOWLEDGED: WarningCode = WarningCode("NOTES_CURSOR_UNACKNOWLEDGED");
/// A PR journal could not be read; reconciliation covers the gap.
pub const PR_JOURNAL_UNAVAILABLE: WarningCode = WarningCode("PR_JOURNAL_UNAVAILABLE");
/// A build context, Git binding or repository was unavailable; build ran locally.
pub const BUILD_CONTEXT_UNAVAILABLE: WarningCode = WarningCode("BUILD_CONTEXT_UNAVAILABLE");
/// A chair switch proceeded on stale or unavailable quota.
pub const CHAIR_SWITCH_QUOTA_STALE: WarningCode = WarningCode("CHAIR_SWITCH_QUOTA_STALE");
/// Note observation indexing failed and will retry.
pub const NOTE_OBSERVATION_DEFERRED: WarningCode = WarningCode("NOTE_OBSERVATION_DEFERRED");
/// A GC sweep lease could not be released.
pub const GC_SWEEP_LEASE_UNRELEASED: WarningCode = WarningCode("GC_SWEEP_LEASE_UNRELEASED");
/// A GC sweep cursor could not be persisted.
pub const GC_SWEEP_CURSOR_UNWRITTEN: WarningCode = WarningCode("GC_SWEEP_CURSOR_UNWRITTEN");
/// Admission slots could not be read; the command proceeds without them.
pub const ADMISSION_SLOTS_UNAVAILABLE: WarningCode = WarningCode("ADMISSION_SLOTS_UNAVAILABLE");
/// A command-line value looks malformed and deserves a second look.
pub const ARGUMENT_VALUE_SUSPECT: WarningCode = WarningCode("ARGUMENT_VALUE_SUSPECT");
/// A private check directory could not be cleaned.
pub const PRIVATE_CHECK_DIR_UNCLEAN: WarningCode = WarningCode("PRIVATE_CHECK_DIR_UNCLEAN");
/// A project or its roots could not be resolved for a hook.
pub const PROJECT_UNAVAILABLE: WarningCode = WarningCode("PROJECT_UNAVAILABLE");
/// A check or local gate could not be run.
pub const CHECK_UNAVAILABLE: WarningCode = WarningCode("CHECK_UNAVAILABLE");
/// Import pins could not be collected for a project.
pub const IMPORT_PIN_COLLECTION_FAILED: WarningCode = WarningCode("IMPORT_PIN_COLLECTION_FAILED");
/// Settled lane worktrees could not be collected.
pub const SETTLED_LANE_COLLECTION_FAILED: WarningCode =
    WarningCode("SETTLED_LANE_COLLECTION_FAILED");
/// Verification could not run and was skipped.
pub const VERIFY_UNAVAILABLE: WarningCode = WarningCode("VERIFY_UNAVAILABLE");
/// The affected-path selection could not be computed.
pub const AFFECTED_SELECTION_UNAVAILABLE: WarningCode =
    WarningCode("AFFECTED_SELECTION_UNAVAILABLE");
/// A temporary checkout could not be removed.
pub const TEMP_CHECKOUT_RETAINED: WarningCode = WarningCode("TEMP_CHECKOUT_RETAINED");
/// A smoke-test scratch project binding could not be removed.
pub const SMOKE_BINDING_RETAINED: WarningCode = WarningCode("SMOKE_BINDING_RETAINED");
/// A build ledger rotated and some rotation entries expired.
pub const BUILD_LEDGER_ROTATED: WarningCode = WarningCode("BUILD_LEDGER_ROTATED");
/// The recent CI duration cache was unavailable; a slower cadence applied.
pub const CI_DURATION_CACHE_UNAVAILABLE: WarningCode = WarningCode("CI_DURATION_CACHE_UNAVAILABLE");
/// An approval carry receipt could not be opened or parsed.
pub const APPROVAL_RECEIPT_UNREADABLE: WarningCode = WarningCode("APPROVAL_RECEIPT_UNREADABLE");
/// A preflight check failed but the launch continues in place.
pub const PREFLIGHT_CONTINUE_IN_PLACE: WarningCode = WarningCode("PREFLIGHT_CONTINUE_IN_PLACE");
/// A refusal notice could not be delivered.
pub const REFUSAL_NOTICE_UNDELIVERED: WarningCode = WarningCode("REFUSAL_NOTICE_UNDELIVERED");
/// A private cache could not be released after collection.
pub const PRIVATE_CACHE_RELEASE_FAILED: WarningCode = WarningCode("PRIVATE_CACHE_RELEASE_FAILED");
/// A steer restart was staggered to avoid a collision.
pub const STEER_RESTART_STAGGERED: WarningCode = WarningCode("STEER_RESTART_STAGGERED");
/// A steer will interrupt the running attempt and launch a successor.
pub const STEER_INTERRUPTS_ATTEMPT: WarningCode = WarningCode("STEER_INTERRUPTS_ATTEMPT");
/// A repository worktree inventory could not be read.
pub const WORKTREE_INVENTORY_UNAVAILABLE: WarningCode =
    WarningCode("WORKTREE_INVENTORY_UNAVAILABLE");
/// An account does not offer the requested model.
pub const ACCOUNT_OFFER_MISMATCH: WarningCode = WarningCode("ACCOUNT_OFFER_MISMATCH");
/// A stored account readiness record is stale.
pub const ACCOUNT_READINESS_STALE: WarningCode = WarningCode("ACCOUNT_READINESS_STALE");
/// An account identity could not be verified; usage is attributed as unknown.
pub const ACCOUNT_IDENTITY_UNVERIFIED: WarningCode = WarningCode("ACCOUNT_IDENTITY_UNVERIFIED");
/// A GitHub login could not be injected from the host environment.
pub const GH_LOGIN_UNAVAILABLE: WarningCode = WarningCode("GH_LOGIN_UNAVAILABLE");
/// GitHub publishing could not be configured.
pub const GH_PUBLISHING_UNAVAILABLE: WarningCode = WarningCode("GH_PUBLISHING_UNAVAILABLE");
/// GitHub publishing credentials were withheld by the access policy.
pub const GH_CREDENTIALS_WITHHELD: WarningCode = WarningCode("GH_CREDENTIALS_WITHHELD");
/// A GitHub read was skipped because the rate limit was reached.
pub const GH_RATE_LIMITED: WarningCode = WarningCode("GH_RATE_LIMITED");
/// Actions runs could not be read for a commit.
pub const ACTIONS_RUNS_UNAVAILABLE: WarningCode = WarningCode("ACTIONS_RUNS_UNAVAILABLE");
/// A remote fetch failed; the current integration resolution was used.
pub const REMOTE_FETCH_FAILED: WarningCode = WarningCode("REMOTE_FETCH_FAILED");
/// Forge discovery failed; local journal sources continue.
pub const FORGE_DISCOVERY_UNAVAILABLE: WarningCode = WarningCode("FORGE_DISCOVERY_UNAVAILABLE");
/// A configured forge source could not be resolved.
pub const FORGE_SOURCE_UNRESOLVED: WarningCode = WarningCode("FORGE_SOURCE_UNRESOLVED");
/// A forge forwarder was unavailable; reconciliation continued.
pub const FORWARDER_UNAVAILABLE: WarningCode = WarningCode("FORWARDER_UNAVAILABLE");
/// Retry admission skipped ambient credentials outside the bounded exact matcher.
pub const RETRY_CREDENTIALS_SKIPPED: WarningCode = WarningCode("RETRY_CREDENTIALS_SKIPPED");

/// A route model is a full `ADAPTER/ID` that the effective catalogue does not
/// know. The declared spelling is preserved; the warning names it.
pub const ROUTE_MODEL_NOT_CATALOGUED: WarningCode = WarningCode("ROUTE_MODEL_NOT_CATALOGUED");

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

    // Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn every_declared_code_names_itself_in_upper_snake_once() {
        // Read the declarations from this file, so a new const is covered
        // without a hand-kept list.
        let source = include_str!("warning.rs");
        let mut seen = std::collections::BTreeSet::new();
        let mut rest = source;
        while let Some(start) = rest.find("pub const ") {
            rest = &rest[start + "pub const ".len()..];
            let Some((name, tail)) = rest.split_once(':') else {
                break;
            };
            let Some(body) = tail.trim_start().strip_prefix("WarningCode =") else {
                continue;
            };
            let value = body
                .split_once("WarningCode(\"")
                .and_then(|(_, after)| after.split_once('"'))
                .map(|(value, _)| value)
                .unwrap_or_default();
            assert_eq!(value, name, "the code string must equal its const name");
            assert!(
                !value.is_empty()
                    && value.bytes().all(|byte| byte.is_ascii_uppercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'),
                "{value}"
            );
            assert!(seen.insert(value.to_owned()), "{value} is declared twice");
        }
        assert!(seen.contains("STEER_TARGET_SETTLED"), "{seen:?}");
        assert!(seen.len() > 50, "the scan found only {seen:?}");
    }
}
