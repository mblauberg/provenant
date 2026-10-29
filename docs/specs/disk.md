# Disk report

Owner: `crates/os/src/disk.rs` (measurement, report shape),
`crates/confinement/src/disk.rs` (lane classes), `crates/run/src/disk.rs`
(run inventory), `src/disk.rs` (composition)
Anchor: `crates/os/src/disk.rs`, `crates/run/src/disk.rs`, `tests/disk_report.rs`

`provenant disk report` inventories lane build caches, the selected
repository's worktrees and the host state root. The JSON fields are
documented on `DiskReport` and `DiskEntry` in `crates/os/src/disk.rs`;
operator guidance is in the [disk inspection runbook](../runbooks/disk.md).
Paths below are relative to `tools/front-door/`.

## Strictly read-only

The report creates no state directories, databases, identities, lock files
or overflow artefacts, and never deletes, repairs, settles or checkpoints.
SQLite may create or update only the `-shm` and `-wal` companion files of an
existing run store while reading it. If the run store has a non-empty rollback journal or an
unavailable index, the report falls back to a filesystem-only inventory with an
error and unknown terminal status. A report over 1 MiB returns
`RESULT_TOO_LARGE` without publishing an artefact.
- Why: inspecting disk use must be safe while lanes run.
- Test: `src/disk.rs::disk_report_inventories_worktrees_and_unindexed_state_without_writes`;
  `crates/run/src/disk.rs::disk_reader_refuses_rollback_journal_without_modification`
- Owner: `src/disk.rs`, `crates/run/src/disk.rs`

## Bounded walks that never follow links

The walk never follows symlinks or crosses a held anchor's device boundary. Each
measured path has a budget of 1,000,000 entries and 10 seconds, and depth is
capped at 256; reaching a limit records one error and keeps the bytes measured so far.
Hard-linked files count once across the report; when roots overlap, allocation is attributed to the first observation.
- Why: a link or a huge tree must not send the report outside its roots or
  hang it.
- Test: `crates/os/src/disk.rs::measurement_never_follows_links_and_separates_tagged_cache`;
  `::walk_budget_preserves_partial_bytes_and_reports_exhaustion`
- Owner: `crates/os/src/disk.rs`

## Figures are observations, not promises

`unshared_bytes` is APFS private size where available and allocated blocks
elsewhere; any fallback sets `method` to `allocated_fallback`. Neither figure
promises bytes recoverable by deletion, and the report is a sequence of
observations, not an atomic snapshot.
- Why: users decide deletions from these numbers.
- Test: `crates/os/src/disk.rs::unavailable_private_size_selects_allocated_fallback`;
  `crates/os/src/sys/fs.rs::returned_attributes_control_private_size`
- Owner: `crates/os/src/disk.rs`, `crates/os/src/fs.rs` (`private_size_at`)

## Classes grant no authority

Lane classification reuses garbage collection (GC) owner parsing and reason policy. Missing or
ambiguous writer evidence stays unknown and protects its cache; measurement
errors classify a cache as `refused`; v1 markers never become idle.
`reclaim: "gc"` and owner or writer facts are hints only, and GC revalidates
its own conditions. The run inventory covers the host state root, never the
invoking lane's `PROVENANT_RUN_STATE_DIR`.
- Why: a report row must never become deletion authority.
- Test: `crates/confinement/src/disk.rs::lane_inventory_refuses_incomplete_measurements`;
  `::lane_classification_reuses_gc_owner_and_reason`
- Owner: `crates/confinement/src/disk.rs`

## Decisions not adopted

The disk-use epic #1570 closes with these decisions:

- No byte budget (`disk.budget_bytes`) or LRU eviction by `last_used` at
  lease release. Free-space triggers cover single-user hosts: the GC free
  floor defaults to 10% of the state volume, at least 50 GiB; GC also runs
  daily, and `gc.alert_below` raises a low-space alert. Lane admission,
  build pauses and pressure collection share `gc.free_floor`. Reopen this
  decision if a host runs out of space with all of these active.
- No seeded warm builds using a host seed cloned into lanes with `clonefile`.
  The [Stage 0 workshop trial](https://github.com/mblauberg/provenant/issues/1570#issuecomment-6407476129) found that Cargo fingerprints the registry source
  path: a private or symlinked `CARGO_HOME` makes every registry crate dirty.
  A seed stays Fresh only with the host `~/.cargo`, and workspace crates still
  rebuild because the lane checkout's mtimes are newer than the seed.
  Re-measure on a quiet machine only if both constraints change. No ADR 0030
  exists in `docs/adr` for seeding; this section records the decision.
- No `git worktree lock` while a lane runs. Writer leases and custody records
  serve that purpose; the lock was dropped during Stage 1.

## Idle declared output and explicit removal

GC may prune a declared directory in a registered linked worktree without a
terminal lane, under free writer custody and a fresh no-live-lane proof.
The directory must be Git-ignored, contain no tracked or kept path, and have
no writes within the idle age (one day under the free floor). The primary
checkout and unknown writers stay protected. JSON separates `idle_output`.
Explicit removal previews all ignored paths before pruning declared output;
undeclared ignored paths refuse removal and appear in the error. Explicit
removal skips the bounded idleness scan but still checks repository boundaries.
Every nested `.git` entry, file or directory, protects its repository and any
declared output containing it, including beneath ignored parents.
- Why: rebuildable output should not require ending a long-lived worktree,
  while ignored secrets and tracked directories remain protected.
- Test: `crates/confinement/src/worktrees.rs::gc_idle_output_requires_ignored_idle_and_free_writer`;
  `::remove_declared_ignored_output_but_names_undeclared_env`
- Owner: `crates/confinement/src/worktrees/gc.rs`, `src/worktrees.rs`

## Opt-in foreign collection

`gc.projects.<id>.foreign_worktrees = "merged-clean"` applies exactly where
GC otherwise reports unknown writer evidence. The default is `"keep"`.
Fetched-default ancestry, idle writes, clean Git state, no live lanes or
reservations, free existing writer locks and a complete free process census
are required. Repository custody protects registration rechecks; every
worktree is verified against the Git common directory. The primary stays
protected and local refs survive this policy's removals.
GC verifies the repository's local `core.bare` value before selecting a primary
checkout. Bare repositories and their linked checkouts cannot supply that root,
even when the bare directory is named `.git`. A primary using a separate Git
directory remains eligible; a linked checkout is never the fallback primary.
Foreign idleness scans the non-output tree within 1,000,000 entries and five
seconds. Declared ignored output contributes only its top-level directory mtime:
the scanner skips its contents, so neither deep writes nor the worktree root
stand in for output age. Root and non-output mtimes independently protect recent
work outside output. Removal still checks nested repository boundaries.
Brief repository custody establishes writer state. Bulk output pruning holds
writer custody only. Exclusive repository custody is reacquired after pruning
for the final recheck and Git removal.
Queued and pending submitted dispatches naming a worktree retain it with
`unknown_writer`, including managed trees without a terminal lane record.
Tasks not yet submitted into a run store are not visible to this census;
keep an external reservation until submission to protect such a tree.
Applied foreign removal creates a private writer lock when state is absent,
and holds it across final checks and Git removal without creating an ownership
identity. Previews do not create state. Native processes do not take this
advisory lock; the complete process census and final clean recheck still apply.
State created solely for this call is removed after removal or refusal, while
the writer lock remains held and repository custody excludes state creation.
Cleanup has an independent five-second repository-lock budget even when the
collection's deadline has expired or its cancellation token has been set.
Existing state survives. Cleanup avoids leaving identity-less directories that
would block a later `worktree create`; creation does not adopt unknown state.
- Why: native worktrees need an explicit alternative to terminal-lane evidence.
- Test: `crates/confinement/src/worktrees.rs::gc_foreign_merged_clean_is_opt_in_idle_and_requires_complete_census`
- Owner: `crates/confinement/src/worktrees/gc.rs`, `crates/config/src/gc.rs`

## Registration hygiene

Scheduled enumeration excludes stale, non-Git and known transient locations
before collecting project IDs for disk alerts. Existing identity records are
retained for indexed evidence. New transient and non-Git invocation locations
use deterministic in-memory identities without persistent project records.
GC preserves `unobserved_projects` as an array of paths. The separate
`skipped_transient` array reports excluded registered common directories,
including those beneath `scratchpads` or a `.provenant-staging.` component.
Explicit GC on a transient checkout reports its common directory and skips
collection. Unregistered transient roots cannot be discovered by a scheduled
registry scan.
- Why: invocation scratch paths must not accumulate as scheduled projects.
- Test: `crates/config/src/project_binding.rs::transient_roots_have_identity_without_persistent_registration`;
  `::scheduled_projects_exclude_missing_non_git_and_old_transient_bindings`
- Owner: `crates/config/src/project_binding.rs`, `src/worktrees.rs`

## Closed-run captures and event logs

`retention.logs_days` defaults to 30 (range 1 to 3650). Under the existing
publication, continuation, reservation and writer checks, GC expires stdout
and stderr captures larger than 64 KiB, and intermediate event log rows, only
when all sibling attempts are terminal. A distinct `.log.tail` file retains
the final 64 KiB. Results, exit evidence, original digest records, small
captures and child-store databases survive. The same policy visits indexed
child stores. Disk inventory reports `attempt_logs`; GC reports
`provider_state.attempt_logs`.
Disk reports compact capture observations by class, preserving byte totals and
capture counts so many closed runs fit within the report limit.
`output --part stderr` falls back to the retained `stderr.log.tail` when
the full capture has expired. JSON reports `complete=false`; text marks the
retained tail as truncated. Offsets and digest describe the retained bytes.
- Why: bound bulky captures and historical logs without rewriting what sealed digests describe.
- Test: `crates/run/src/retention.rs::gc_logs_keep_tail_and_evidence_and_obey_age_custody_and_preview`;
  `::gc_expires_closed_run_events_after_log_retention`;
  `::gc_logs_refuse_symlinks`; `crates/run/src/disk.rs::disk_usage_rolls_many_attempts_up_per_run`
- Owner: `crates/run/src/retention.rs`, `crates/run/src/disk.rs`

## Pressure collection and daily backstop

Chair session start and successful checkpoint saves warn when free space on the
state, checkout or lane Cargo cache volume is below twice `gc.free_floor`,
capped at half that volume's capacity. The warning identifies the volume,
reports free space and the threshold and names `provenant disk report` and
`provenant gc`. Volume sampling does not walk filesystem trees; failures and
timeouts are silent and do not affect the hook or save. Reclaimable space is
not estimated. Lane sessions do not receive the warning.
- Why: chairs need headroom to investigate disk use before admission refuses.
- Test: `session_start_warns_once_only_when_disk_is_below_headroom`,
  `session_save_warns_once_only_when_disk_is_low`, and disk guard unit tests.
- Owner: `src/disk_guard.rs`, `src/hooks.rs`, `src/lib.rs`, `crates/cli/src/workflow.rs`

Admission, running guardians and settlement sample the minimum free space
across the host state, lane working directory and lane Cargo cache volumes.
Missing cache directories use their nearest existing ancestor. Each observation
re-reads `gc.free_floor`, including percentage recalculation, so a configuration
change applies to queued and running attempts on their next observation.
Admission tickets retain the submission-time floor as historical evidence.
An unreadable floor or volume keeps admission queued until a valid observation
or its deadline; a guardian retains its last pause decision.

Guardians durably record paused native identities in the attempt's
`disk-paused.json` before each STOP. Recovery resumes those identities after
guardian death, including when ancestry or exit evidence is gone, and resumes
owned orphans before TERM. Settlement also consumes the pause journal. A failed
disk sample keeps the last pause decision; cancellation and provider exit resume
paused builds. Pause status is shown only while the recorded guardian is alive.

Writer admission makes one collection attempt bounded to five seconds before
refusing low disk, then rechecks the worktree, state and cache volumes.
Refusals include reported reclaimed bytes. A timeout can remove data without
returning a byte report. Pressure collection still prunes declared idle output,
using the one-day idle floor when below the free floor. It skips Git fetches and
worktree deletion and honours `gc.schedule=false`. Admission-triggered collection
selects only the launching project, including its caches and temporary run
payloads; it does not enumerate other registered projects. Applied sweeps share
a nonblocking collection lock. The host tick runs at load and every minute; collection is a daily tick task.
Manual `gc --pressure` reports one notice line when `gc.schedule=false`, a
transient project is skipped or no eligible data is in scope. JSON includes
the same reason in `notice`.
Settlement already prunes provider caches and temporary state; removal prunes
the removed worktree's cache. Settlement lane-cache and delivery-integration
event wiring remains with those callers.
- Why: free reclaimable space before sending the chair a manual remedy.
- Test: `crates/cli/src/run.rs::writer_rechecks_once_after_collection_and_reports_reclaimed_bytes`;
  `src/tick_agent.rs::plist_renders_interval_arguments_state_logs_and_xml_escaping`;
  `src/disk_guard.rs::settlement_collects_below_the_shared_floor_only`;
  `::lane_pause_rereads_the_floor_each_observation`;
  `crates/run/src/dispatch_tests.rs::admission_rereads_gc_floor_after_submission_and_while_waiting`;
  `crates/confinement/src/disk.rs::lane_sample_uses_each_output_volume_and_propagates_errors`
- Owner: `crates/cli/src/run.rs`, `src/worktrees.rs`, `src/tick_agent.rs`
