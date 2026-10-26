# Disk report

`provenant disk report` inventories lane build caches, the selected repository's
worktrees and the resolved host state root. It accepts the existing `--project`,
`--state-dir` and `--json` globals. It creates no state directories, databases,
identities, lock files or overflow artefacts, and never deletes, repairs or
settles state. SQLite may create or update only `run.sqlite3-shm` and
`run.sqlite3-wal` companions of an existing store while reading its live index.

## Measurement

The OS crate owns `Directory::private_size_at`. On macOS it calls
`getattrlistat` with `ATTR_CMN_RETURNED_ATTRS`, extended common attribute
`ATTR_CMNEXT_PRIVATESIZE` in `forkattr`, and
`FSOPT_ATTR_CMN_EXTENDED | FSOPT_NOFOLLOW`. A returned-attribute bitmap determines
whether the size is present. Unsupported filesystems and other platforms use
allocated blocks (`st_blocks * 512`). Genuine observation failures also appear
in `errors`.

`allocated_bytes` records allocated blocks. `unshared_bytes` records APFS private
bytes where available and allocated blocks elsewhere. `method` is
`apfs_private_size` when all observations provide private sizes; any fallback
sets it to `allocated_fallback`. A fallback is an allocation estimate, without
APFS clone or snapshot accounting. Neither figure promises bytes recoverable by
deleting an entry.

The walk does not follow symlinks or cross a held anchor's device. It measures
`target` contents, including targets nested in attempt temporary directories,
without parsing Cargo build artefacts. Each measured path has a budget of
1,000,000 entries and ten seconds, checked between filesystem operations; depth
is capped at 256. Reaching an entry or time limit records one error per walk
and preserves partial bytes. Filesystem limits, including available file
descriptors, can stop traversal earlier with an error.
These limits cannot interrupt an individual blocked filesystem call.
`newest_write_ns` is the newest observed mtime in nanoseconds since the Unix
epoch. Entries deleted between enumeration and observation are skipped without
an error; other observation failures remain errors.

One `(device, inode)` set spans the report for multiply linked non-directories,
counting hard-linked files once. Duplicate names still contribute their
modification time. Directory names are streamed rather than collected and
sorted. Nested worktrees are measured first; their roots are skipped when
measuring the containing worktree. Overlap comparisons canonicalise trusted
parent paths, so symlinked ancestors identify the same worktree without
following links beneath an anchor. Later inventories inside a measured worktree
also skip those bytes: allocation belongs to the first observation. They inherit
its newest-write timestamp conservatively, and an incomplete earlier walk
remains an error. A tagged Cargo cache in worktree or other
state is separated from its containing entry. Attempt temporary and retained
entries keep their nested caches in their own byte totals. Lane caches split
only immediate children into `target`, `cargo_home` (the `cargo-home` directory)
and `other`; these parts use unshared bytes and sum to the lane's unshared total.

## Classes

| Class | Facts |
| --- | --- |
| `lane_cache` | State, owner when recognised, top-level parts, optional `reclaim: "gc"` |
| `worktree` | Branch when attached, writer `live`, `free` or `unknown`, merge containment when known |
| `host_target` | Directory containing a regular `CACHEDIR.TAG` in worktree or other state; owner worktree and writer when measured inside a worktree |
| `attempt_tmp` | Attempt `tmp`; `leak: true` when its indexed attempt is terminal, `false` while nonterminal |
| `provider_state` | One class aggregate for attempt provider-state, separately from retained evidence |
| `attempt_retained` | One class aggregate for scratch, provider-history, quarantine, logs, stdout and stderr |
| `child_store` | One class aggregate for `children/<attempt>`, including nested stores |
| `other_state` | One class aggregate for remaining host, run and attempt state |

Owner and writer facts on `host_target` rows grant no removal authority.

Lane states are `live`, `idle`, `orphaned`, `foreign`, `legacy`, `tombstone`,
`refused` and `unmarked`. Classification reuses cache-owner parsing and GC
reason policy, with the same seven-day default idle threshold. Foreign owners
are outside the selected project's worktree inventory. A valid v1 marker is
orphaned when its owner path is missing, foreign when an existing owner is
outside the project, and legacy otherwise. V1 markers never become idle.
Missing, malformed or unverifiable markers are retained as unmarked, legacy
or refused. Worktree writer observations use existing registration and lock
files opened read-only, plus the native process census. Missing or ambiguous
evidence stays unknown and protects its cache. Merge containment is against
the main worktree's observed HEAD, including when invoked from a linked
worktree. The main worktree omits
`merged` and does not run a merge-containment check against itself.

Target contents contribute to the newest-write observation, so a fully measured
cache containing a target can be idle. `reclaim: "gc"` is an ownership hint;
GC revalidates its own removal conditions.
Measurement errors classify a lane cache as `refused`, preserving its partial
byte observations without claiming eligibility for removal.

The run inventory covers the resolved host state, including nested child stores;
it does not switch to the invoking lane's `PROVENANT_RUN_STATE_DIR`. Terminal
facts come from a normal SQLite read-only connection, including committed WAL
frames while writers remain connected. SQLite shared-memory read marks and WAL
companions are allowed only beside an existing store; the database itself is
never created or written. A nonempty rollback journal or unavailable index
triggers a filesystem-only inventory with an error and unknown terminal status.
An unknown leak field is omitted. The command never checkpoints a database to
obtain fresher facts.

## JSON contract

The existing `CommandResult` envelope has `command: "disk report"`. Its `data`
contains the following additive schema (example with one lane):

```json
{
  "schema_version": 1,
  "method": "apfs_private_size",
  "volume": {"path": "/state", "available_bytes": 0},
  "entries": [{
    "class": "lane_cache", "state": "orphaned", "path": "/cache/lane",
    "owner": "/repo/.worktrees/old", "allocated_bytes": 0,
    "unshared_bytes": 0, "newest_write_ns": 0,
    "parts": {"target": 0, "cargo_home": 0, "other": 0}, "reclaim": "gc"
  }],
  "totals": {"lane_cache": {"count": 1, "allocated_bytes": 0, "unshared_bytes": 0}},
  "errors": []
}
```

`volume.path` is the existing state directory or its nearest existing ancestor;
`available_bytes` is unprivileged available space on that filesystem. Other
inventoried roots may reside on different volumes. Totals aggregate entries by
class. Retained, other state and child-store rows are folded into one aggregate
per class after inventory. Its `path` is the common ancestor of the folded
paths; `count` is the number of inventory rows represented, rather than files.
Retained output contributes one row per run, child stores one per child, and
other state one per run plus one for the shared host-root metadata. Class total
counts sum these represented rows, and byte totals are unchanged by folding.
Temporary directories, caches and worktrees retain their individual paths for
investigation.
Optional class-specific fields are omitted when unknown or inapplicable.
Per-path errors accompany successful partial reports; root-resolution failures
use an error envelope. JSON output over 1 MiB returns `RESULT_TOO_LARGE` without
publishing an artefact. Human output uses the existing escaped terse renderer.

The report is a sequence of observations, not an atomic filesystem snapshot.
Review `errors` and the measurement limits before interpreting totals. See the
[disk inspection runbook](../runbooks/disk.md).
