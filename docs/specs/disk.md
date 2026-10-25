# Disk report

`provenant disk report` inventories lane build caches, the selected repository's
worktrees and the resolved host state root. It accepts the existing `--project`,
`--state-dir` and `--json` globals. It creates no state, identities, lock files or
overflow artefacts, and never deletes, repairs, settles or changes files.

## Measurement

The supervisor owns `Directory::private_size_at`. On macOS it calls
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

The walk does not follow symlinks or cross a held anchor's device. It stops at
every directory named `target`: the directory node is measured, its contents
are excluded. Thus target figures and totals are partial, often much smaller
than the full build footprint. `newest_write_ns` is the newest observed mtime
in nanoseconds since the Unix epoch, also excluding target contents. Walk depth
is capped at 256 and failures preserve partial results.

One `(device, inode)` set spans the report, counting hard-linked files once.
Duplicate names still contribute their modification time. Nested worktrees are
measured before their containing worktree. A tagged Cargo cache is separated
from its containing entry. Lane caches split only immediate children into
`target`, `cargo_home` (the `cargo-home` directory) and `other`; these parts use
unshared bytes and sum to the lane's unshared total.

## Classes

| Class | Facts |
| --- | --- |
| `lane_cache` | State, owner when recognised, top-level parts, optional `reclaim: "gc"` |
| `worktree` | Branch when attached, writer `live`, `free` or `unknown`, merge containment when known |
| `host_target` | Directory containing a regular `CACHEDIR.TAG`, outside lane-cache splits |
| `attempt_tmp` | Attempt `tmp`; `leak: true` when its indexed attempt is terminal, `false` while nonterminal |
| `attempt_retained` | Scratch, provider-state, quarantine, logs, stdout and stderr |
| `child_store` | One entry per `children/<attempt>`, including its nested store |
| `other_state` | Remaining host or run-store state |

Lane states are `live`, `idle`, `orphaned`, `foreign`, `legacy`, `tombstone`,
`refused` and `unmarked`. Classification reuses cache-owner parsing and GC
reason policy, with the same seven-day default idle threshold. Foreign owners
are outside the selected project's worktree inventory. Missing, malformed or
unverifiable markers are retained as unmarked, legacy or refused. Worktree
writer observations use existing registration and lock files opened read-only,
plus the native process census. Missing or ambiguous evidence stays unknown and
protects its cache. Merge containment is against the main worktree's observed
HEAD, including when invoked from a linked worktree.

Because target contents are excluded, the report cannot prove that a cache
containing a target is idle. It conservatively retains that cache as `live`
unless another GC reason, such as an orphaned owner or tombstone, applies.
`reclaim: "gc"` is an ownership hint; GC revalidates its own removal conditions.
Measurement errors classify a lane cache as `refused`, preserving its partial
byte observations without claiming eligibility for removal.

The run inventory covers the resolved host state, including nested child stores;
it does not switch to the invoking lane's `PROVENANT_RUN_STATE_DIR`. Terminal
facts come from an immutable SQLite main-database read. Nonempty WAL or rollback
journals trigger a filesystem-only inventory with an error and unknown terminal
status. This avoids SQLite shared-memory writes. An unknown leak field is
omitted. The command never checkpoints a database to obtain fresher facts.

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
class. Optional class-specific fields are omitted when unknown or inapplicable.
Per-path errors accompany successful partial reports; root-resolution failures
use an error envelope. JSON output over 1 MiB returns `RESULT_TOO_LARGE` without
publishing an artefact. Human output uses the existing escaped terse renderer.

The report is a sequence of observations, not an atomic filesystem snapshot.
Review `errors` and the measurement limits before interpreting totals. See the
[disk inspection runbook](../runbooks/disk.md).
