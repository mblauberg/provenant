# Disk report

Owner: `crates/os/src/disk.rs` (measurement), `crates/confinement/src/disk.rs`
(lane classes), `crates/run/src/disk.rs` (run inventory), `src/disk.rs`
(composition)
Anchor: `crates/os/src/disk.rs`, `crates/run/src/disk.rs`

`provenant disk report` inventories lane caches, worktrees and the host state
root; `DiskReport` documents the fields and the [disk and GC runbook](../runbooks/gc.md)
the procedure. Paths are relative to `tools/front-door/`.

## Strictly read-only

The report creates, deletes, repairs or settles nothing; SQLite may touch only
an existing store's `-wal` and `-shm` companions. An unreadable run store falls
back to a filesystem-only inventory with an error.
- Why: inspecting disk use must be safe while lanes run.
- Test: `src/disk.rs::disk_report_inventories_worktrees_and_unindexed_state_without_writes`;
  `crates/run/src/disk.rs::disk_reader_refuses_rollback_journal_without_modification`

## Bounded walks that never follow links

The walk never follows symlinks or crosses a device boundary, and each measured
path has an entry, time and depth budget; reaching one records an error and
keeps the bytes measured so far. Hard links count once.
- Why: a link or a huge tree must not send the report outside its roots or hang it.
- Test: `crates/os/src/disk.rs::measurement_never_follows_links_and_separates_tagged_cache`;
  `::walk_budget_preserves_partial_bytes_and_reports_exhaustion`

## Figures are observations, not promises

No figure promises bytes recoverable by deletion, a fallback measurement says
so in `method`, and the report is a sequence of observations, not a snapshot.
- Why: users decide deletions from these numbers.
- Test: `crates/os/src/disk.rs::unavailable_private_size_selects_allocated_fallback`

## Classes grant no authority

Lane classes reuse GC's owner and reason policy. Missing or ambiguous writer
evidence stays unknown and protects its cache. A `reclaim` hint never becomes
deletion authority: GC revalidates its own conditions.
- Why: a report row must never become deletion authority.
- Test: `crates/confinement/src/disk.rs::lane_inventory_refuses_incomplete_measurements`;
  `::lane_classification_reuses_gc_owner_and_reason`

## Collection keeps evidence and needs fresh proof

GC removes a worktree, or declared idle output inside one, only under held
writer custody with a fresh complete census; unknown writer evidence keeps it
unless the project opts into `merged-clean` foreign collection. Log retention
expires bulky captures only after every sibling attempt is terminal and keeps
the tail, results, exit evidence and digests. Low-disk collection is bounded and
never fetches or deletes worktrees.
- Why: rebuildable output should be reclaimable without ending a worktree, while
  sealed evidence and possible writers stay protected.
- Test: `crates/confinement/src/worktrees/lifecycle_tests/gc_safety.rs::gc_idle_output_requires_ignored_idle_and_free_writer`;
  `::gc_foreign_merged_clean_is_opt_in_immediate_and_requires_complete_census`;
  `crates/run/src/retention.rs::gc_logs_keep_tail_and_evidence_and_obey_age_custody_and_preview`;
  `crates/cli/src/run/tests/writer_disk.rs::writer_rechecks_once_after_collection_and_reports_reclaimed_bytes`
- Owner: `crates/confinement/src/worktrees/gc.rs`, `crates/run/src/retention.rs`
