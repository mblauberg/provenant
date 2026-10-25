# Inspect disk use

Use this procedure to inspect Provenant storage before deciding whether to run
an existing cleanup command. It requires read access to the selected checkout,
host state and lane-cache directories. The report itself never changes files.

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
   does not run it. Foreign, legacy, refused and unmarked caches require owner
   investigation. An `attempt_tmp` entry with `leak: true` belongs to an indexed
   terminal attempt. Scratch, provider state, quarantine and logs are retained
   output, so their presence alone is not a leak.

Every `target` directory is a traversal boundary. Its contents are absent from
the figures, including `parts.target`; these are partial observations rather
than full build-cache sizes. A worktree's `merged` fact describes containment in
the main checkout's observed HEAD and grants no removal authority.

If a journal prevents terminal-state inspection, expect an error and omitted
`leak` fields. Let ordinary run-store owners finish their work, then report
again. The report does not checkpoint, repair or settle state. Permission,
cross-device or depth errors also make totals partial. A result over 1 MiB
fails without writing an overflow artefact; select a narrower project or state
root where appropriate.

No rollback is needed because reporting changes no files. Preserve the JSON in
your own chosen destination if evidence is required; investigate remaining
errors with the owner of each reported path.
