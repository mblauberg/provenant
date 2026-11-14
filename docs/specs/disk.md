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
so, and the report is a sequence of observations, not a snapshot.
- Why: users decide deletions from these numbers.
- Test: `crates/os/src/disk.rs::unavailable_private_size_selects_allocated_fallback`

## Classes grant no authority

Lane classes reuse GC's owner and reason policy. Missing or ambiguous writer
evidence stays unknown and protects its cache. A `reclaim` hint never becomes
deletion authority: GC revalidates its own conditions.
- Why: a report row must never become deletion authority.
- Test: `crates/confinement/src/disk.rs::lane_inventory_refuses_incomplete_measurements`;
  `::lane_classification_reuses_gc_owner_and_reason`

Collection belongs to [worktrees](supervisor.md#worktrees) and
[state retention](state.md#retention-keeps-evidence).

## Dispatch and new-build pressure

Dispatch and new builds share `GcSettings::disk_limits` and `DiskLimits::level`.
Below `gc.free_floor`, warn and proceed. Only below `gc.critical_free` (default
2 GiB; `0B` disables it) refuse new work. Diagnostics report available bytes,
both thresholds and their configuration or shipped-default sources. Running
builds continue. Advisory policy strengths cannot turn the free floor into a
refusal or bypass the separate critical threshold.
- Why: a collection target must not prevent ordinary lane work; critical
  exhaustion can compromise output integrity.
- Test: `crates/config/src/gc.rs::disk_limit_tests::disk_levels_follow_the_declared_thresholds_and_keep_their_sources`;
  `crates/run/src/dispatch_disk_admission_tests.rs::admission_uses_only_the_critical_threshold_for_refusal`;
  `tests/build_slot.rs::disk_policy_applies_before_starting_a_new_build`
