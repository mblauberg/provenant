# Inspect disk use

Use this procedure to inspect Provenant storage before deciding whether to run
an existing cleanup command. It requires read access to the selected checkout,
host state and lane-cache directories. The report does not change durable
state. SQLite may create or update `run.sqlite3-shm` and `run.sqlite3-wal`
companions of an existing store while reading its live index.

1. Select the checkout and request the report:

   ```sh
   provenant --project /absolute/path/to/repo disk report
   ```

   Output lists classes, paths, allocated bytes, observed unshared bytes and
   totals. An absent state directory remains absent.

2. For scripts, request the existing JSON envelope:

   ```sh
   provenant --project /absolute/path/to/repo disk report --json
   ```

   Inspect `data.errors` first. `data.method: "allocated_fallback"` means at
   least one observation uses allocated blocks rather than APFS private bytes.
   Check `data.volume.available_bytes` for space on the state filesystem.

3. Interpret classifications using the [disk specification](../specs/disk.md).
   A lane entry with `reclaim: "gc"` names the existing GC owner; the report
   does not run it. V1 markers with missing owners are orphaned and eligible for
   GC; existing project owners stay legacy and never become idle. Foreign,
   legacy, refused and unmarked caches require owner investigation.
   An `attempt_tmp` entry with `leak: true` belongs to an indexed
   terminal attempt. `provider_state` measures resumable provider state
   separately; [GC](gc.md) applies its terminal retention policy. Scratch,
   archived provider history, quarantine and logs remain retained output.
   Retained output, other run metadata and child stores are rolled up into one
   row per class. Each
   aggregate's `count` records the number of inventory rows it represents.
   Temporary directories, caches and worktrees keep their individual paths.

4. Preview cleanup with `provenant gc --json`; apply it with
   `provenant gc --apply --json`. In addition to lane caches, GC removes clean
   merged worktrees using fetched upstream ancestry. Projects can declare
   regenerable directory names and keep paths in `[gc.projects.PROJECT_ID]`.
   Only known terminal lanes qualify for output pruning, and writes within
   24 hours are kept. See the [configuration runbook](configuration.md#lane-cache-collection).
   Set `gc.alert_below` for a Fabric event and gc log message once per low-space
   dip. GC does not sweep arbitrary `/private/tmp/provenant-*` paths.

Every `target` directory is sized, including nested attempt targets and
`parts.target` in lane caches. Build artefacts are never parsed. Each measured
path has a budget of 1,000,000 entries and ten seconds, checked between
filesystem operations; depth is capped at 256. Reaching a limit reports an
error and the bytes observed so far. Files that disappear during the walk are
skipped without an error. A worktree's
`merged` fact describes containment in the main checkout's observed HEAD and
grants no removal authority. The main worktree omits this field.

Live WAL data is included in terminal-state inspection. If a rollback journal
or an index error prevents inspection, expect an error and omitted
`leak` fields. Let ordinary run-store owners finish their work, then report
again. The report does not checkpoint, repair or settle state. Permission,
cross-device, depth or walk-budget errors also make totals partial. A result
over 1 MiB fails without writing an overflow artefact. The run inventory covers
the whole resolved host state root, so narrowing the selected project does not
reduce its rows. Select a different state root only when it is the intended
inventory scope.

No rollback is needed because reporting changes no durable state. Preserve the
JSON in your own chosen destination if evidence is required; investigate
remaining errors with the owner of each reported path.
