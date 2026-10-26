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
SQLite may create or update only the `-shm` and `-wal` companions of an
existing run store while reading it; a non-empty rollback journal or
unavailable index falls back to a filesystem-only inventory with an error and
unknown terminal status. Output over 1 MiB returns `RESULT_TOO_LARGE`
without publishing an artefact.
- Why: inspecting disk use must be safe while lanes run.
- Test: `src/disk.rs::disk_report_inventories_worktrees_and_unindexed_state_without_writes`;
  `crates/run/src/disk.rs::disk_reader_refuses_rollback_journal_without_modification`
- Owner: `src/disk.rs`, `crates/run/src/disk.rs`

## Bounded walks that never follow links

The walk never follows symlinks or crosses a held anchor's device. Each
measured path has a budget of 1,000,000 entries and ten seconds, and depth is
capped at 256; reaching a limit records one error and keeps partial bytes.
Hard-linked files count once across the report, and allocation belongs to
the first observation when roots overlap.
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

Lane classification reuses GC's owner parsing and reason policy. Missing or
ambiguous writer evidence stays unknown and protects its cache; measurement
errors classify a cache as `refused`; v1 markers never become idle.
`reclaim: "gc"` and owner or writer facts are hints only, and GC revalidates
its own conditions. The run inventory covers the host state root, never the
invoking lane's `PROVENANT_RUN_STATE_DIR`.
- Why: a report row must never become deletion authority.
- Test: `crates/confinement/src/disk.rs::lane_inventory_refuses_incomplete_measurements`;
  `::lane_classification_reuses_gc_owner_and_reason`
- Owner: `crates/confinement/src/disk.rs`
