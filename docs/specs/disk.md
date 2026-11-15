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

Submission, admission and new builds take one decision, `DiskGate::decide`:
the thresholds from `GcSettings::disk_limits` plus the effective strength of
the declared `DISK_FLOOR` policy rule. Below `gc.free_floor`, warn and proceed.
Below `gc.critical_free` (default 2 GiB; `0B` disables it) `DISK_FLOOR` decides:
it warns and proceeds by default, for every lane class; `policy.DISK_FLOOR =
"refuse"` holds new work and `"off"` silences it. Messages report available
bytes, both thresholds, their configuration or shipped-default sources and the
`provenant gc` remedy. Running builds continue. The build shim receives the
host's serialized gate in `PROVENANT_BUILD_DISK_LIMITS` when `PortTable.build_admission`
selects builtin admission. The shim receives its selected backend and effective source
in `PROVENANT_BUILD_ADMISSION_BACKEND` and `PROVENANT_BUILD_ADMISSION_SOURCE`.
A command backend streams the repository wrapper around Cargo; `none` skips
build admission. Both retain the independent local-Cargo execution policy. Builtin sandbox-off preparation
takes the same decision against its target volume before launch. A record that
cannot be written still fails as a data-integrity error, outside this decision.
- Why: low space is not one of the four hard boundaries
  ([ADR 0035](../adr/0035-four-hard-boundaries-and-flexible-everything-else.md)); a collection target must not
  prevent ordinary lane work, and a host that wants a hard floor declares it.
- Test: `crates/config/src/gc.rs::disk_gate_effect_follows_the_declared_disk_floor_rule`;
  `crates/run/src/dispatch_disk_admission_tests.rs::admission_follows_the_declared_disk_floor_policy`;
  `crates/cli/src/run/tests/writer_disk.rs::writer_disk_check_follows_the_declared_disk_floor_policy`;
  `tests/build_slot.rs::disk_policy_applies_before_starting_a_new_build`
