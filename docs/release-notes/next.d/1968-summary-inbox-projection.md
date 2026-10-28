## Additions

- Include the addressed chair session in local overview inbox rows.
- Record structured checkpoint summary metadata in chair discovery when available.
- Publish checkpoint history and discovery metadata for guarded field edits.
- Add revision-guarded chair summary reads and field updates.
- Compare retained chair summary revisions, defaulting `--to` to the current revision and reporting added and removed request IDs.
- Reconcile summary requests with the bounded local inbox, reporting unknown references when coverage or attribution is incomplete.
- Capture local attempt participation at registered worktree dispatch, continuation, PR creation and landing. Retain links through GC and worktree removal, and list role, model, account and allocation with `provenant pr contributions N [--json]`. Isolate reused branch generations after observed PR closure, flag ambiguous multi-PR participation and truncated links, and retain run, outcome and timestamps. Read-only work uses the `read_only` role. Binding failures warn without failing PR creation or landing; retry with `provenant pr contributions N --bind`. Read legacy rows without migration, and report unregistered projects as unbound. Unknown facts and missing peer coverage remain explicit.
- Sweep stale PR bindings against GitHub before binding a reused branch, including during PR creation and closed-PR retries. Capture stays offline; attempts captured before the sweep retain their existing links. Warn when closure finds no open binding. Skip attempts with sealed allocations during automatic binding and report their count as `coverage.skipped_sealed` with `skipped` coverage, while continuing to bind other attempts. Closed bindings retain their observed head and cannot resume tracking after a PR is reopened.
