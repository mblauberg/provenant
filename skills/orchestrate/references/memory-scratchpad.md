# Memory & scratchpads

For a long, layered run, use the **filesystem as shared memory**. For the worker→synthesiser pattern,
files + a manifest are usually enough and auditable. Separate four concerns:

- **scratchpad:** temporary notes and worker artifacts inside the run;
- **manifest:** index of artifacts and status;
- **durable resume state:** compact state needed by the next session;
- **observability:** what agents/tools ran, failed, or disagreed.

Reach for a memory framework only when you genuinely need temporal fact-tracking, large retrieval, or
concurrent-write consistency.

## Run directory layout

`scripts/run_dir_init.sh` scaffolds:

```
<run-dir>/
  MANIFEST.md          # the index: one row per artifact
  findings/            # worker outputs, namespaced + append-only
  crossfamily/         # verifier / red-team outputs
  traces/              # dispatches, failovers, objective checks, disagreements
  SYNTHESIS.md         # the orchestrator's merged result
  FINAL_GATE.md        # completion checklist; fail closed if any required gate is missing
  decisions.md         # resolved calls + unresolved-for-user items
```

## Manifest schema

One row per artifact so any layer can find prior work without re-reading everything:

```
| id | path | topic | produced_by | date | status | retention | supersedes |
```

`status` ∈ `draft | verified | superseded | retired`; `retention` ∈ `capsule | evidence | ephemeral`.
`supersedes` points at the artifact ID a newer row replaces.

## Final gate minimum rows

`FINAL_GATE.md` should include P0/P1 findings triaged, objective anchors, cross-family verifier
guarantee, `CROSS-FAMILY-NOT-RUN` exceptions, and high-stakes/low-oracle coverage. A cross-family
verifier certifies only when `status=ok`, `cross_family=true`, and
`read_only_guarantee=enforced/oauth_safe_mode`; best-effort results are scout signals unless a user
accepts the weaker guarantee.

## Rules

- **Namespacing:** each worker owns its own file(s); never two writers on one file concurrently.
- **Append-only** within a run; corrections create a *new* artifact that `supersedes` the old, rather
  than mutating it — preserves provenance.
- **Keep pointers:** every finding records the source (path/URL/line) behind its claims.
- **Curate (the neglected phase):** writing and reading get built; dedup / contradiction-resolution /
  retirement get skipped, and append-only stores then degrade. Periodically mark stale or contradicted
  rows `retired`/`superseded`, and validate that manifest paths still exist. Budget time for this.
- **Close the run:** graduate durable conclusions, retain the manifest/synthesis/gate and failed-leg
  receipts, then remove only raw payload created by this run and no longer referenced. Use the
  `session` skill's context-hygiene contract; never delete unknown or
  pre-existing untracked files.

## Cross-session handoff

To resume later, a session needs only the `MANIFEST.md` + `SYNTHESIS.md` + `FINAL_GATE.md` +
`decisions.md` — not the raw findings. Write a one-line "state of play" at the top of `decisions.md` so
the next session reloads the situation cheaply. If the host tool has its own persistent memory (e.g. a
project `MEMORY.md`), record only durable, non-obvious decisions there — not the whole run.

## Terminalisation and retention

Every bounded run ends `ok`, `failed` or `cancelled` in `RUN_RECEIPT.json`. Readers still accept `succeeded` in retained receipts written before this change.
Run `provenant clean` for repository retention; it protects live, pinned and
referenced capsules and lists unknown paths for triage.
Use `scripts/run_dir_finalize.py`; successful runs require closed gates, valid
manifest paths and no unlisted payload. Failed/cancelled runs retain a reason
and useful partial evidence. `--prune-ephemeral` is dry-run; `--apply` removes
only manifest-classified, retired/superseded ephemeral payload that no capsule
references. Unknown files block successful closure and are never pruned.

## Context management

Prefer reversible compression: summaries must point back to paths/URLs/line ranges. If compaction drops
raw tool output, the raw artifact should still be recoverable through the manifest or session log. This
matches current long-horizon agent guidance: keep the smallest high-signal context in the model window,
while durable state lives outside it.

Research anchors: Anthropic context-engineering guidance (2025) on compaction, structured notes, and
sub-agent architectures; Anthropic managed-agents guidance (2026) on recoverable session state and
harness assumptions that drift.
