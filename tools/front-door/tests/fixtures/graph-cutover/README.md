# Graph cutover fixtures

`read-baseline.json` contains the pre-cut binary's JSON replies for generated
success, failure, quota fallback, committed writer continuation, cancellation,
timeout, delivery, peer receiver and nested-child records. The baseline is
Provenant 1.6.10, source digest
`sha256:45cacca77dfc8a0249665b25262bd2da1632017503ea6a21ae8cdee94ec45b96`;
`provenance.json` records its executable digest. No user run directory was used.

`tests/graph_cutover.rs#seed` creates each record through `RunStore::create`
and `RunStore::publish_terminal`, using synthetic supervisor exit evidence.
It retains the old request schema's empty `fallback` list as an extension;
the pre-cut reader requires that field. The current kernel creates the graph's
initial checkpoint before the fixture installs its terminal observations.
Writer records retain their predecessor and committed head, delivery records
have a delivery receipt, nested records retain a parent attempt, depth and
chair, and peer receiver executions retain the origin graph binding and host.

This corpus checks persisted projections. It does not simulate a transport,
launch a provider or replace lifecycle tests. Those paths are exercised by
`tests/graph_parity.rs#quota_stopped_task_and_composite_member_keep_the_same_lineage`,
`tests/graph_provider.rs#partial_quota_alias_fixture`,
`src/fabric/interface.rs#child_launch_retains_writer_access_and_has_a_stable_logical_identity`
and `src/fabric/peer.rs#peer_submit_graph` and its tests.

The normal test compares current replies with the frozen pre-cut replies.
To also replay the pre-cut reader, supply its immutable executable explicitly:

```sh
PROVENANT_GRAPH_CUTOVER_BASELINE=/path/to/pre-cut/provenant \
  cargo test --locked -p provenant --test graph_cutover
```

`PROVENANT_GRAPH_CUTOVER_CAPTURE=$TMPDIR/cutover-capture` additionally writes
the verified pre-cut capture there. The fixture normalises only the generated
project identity and scratch path prefix; run and attempt IDs, state, outcome,
exit code, result bytes, digests, lineage, routes and delivery fields stay exact.

## Field mapping

`generated_cutover_status_and_output_corpus` compares retained status fields
after these structural mappings. Output checks identity, settlement, artifact
bytes and range metadata, paths, lineage, routes and delivery against the
capture, and validates the current reply schema while allowing added fields. A changed state, exit code or result is
a failure, including a lost result path or staleness observation.

| Pre-cut surface or field | Current surface or field | Reason |
|---|---|---|
| `lanes --id ID --all --json` | `status ID --json` | One status surface. |
| `data.lanes[]` | `data.rows[kind=attempt]` | One row schema for graph, node and attempt. |
| Bare `attempt_id` | `HOST/attempt_id` | Explicit host qualification; the same attempt ID is retained. |
| No row `id`, `kind`, `graph_id`, `node_id` | Added row identifiers and provenance | Graph ownership is explicit. |
| No `graph_state` | Added `graph_state` | Kernel state accompanies the retained provider state. |
| No `outcome`, `cause_code`, `next` | Added outcome, cause and action | Graph outcome and recovery metadata accompany the retained lane fields. |
| `lanes`-only `handoff: null` | Graph attempt predecessor evidence | The removed policy owner has no handoff in these fixtures; a nonempty old handoff fails this comparison. |
| List envelope metadata (`next_cursor`, `capacity_cooldowns`, `review_exempt`) | Targeted row envelope (`timed_out`) | Selection envelope changed; comparisons cover each selected attempt and its command exit code. |
| `output ID --json` data | Retained read contract plus current schema | Added settlement evidence does not freeze the output member set. |

## Pre-cut terminal checkpoint

`old-terminal-checkpoint.json` was written by the same pre-cut executable,
running a single deterministic budget gate. Only `execution.directory` was
rebound to `/synthetic/graph-cutover`, then the envelope digest was recomputed.
The checkpoint payload is unchanged and its digest is checked against
`provenance.json` before the current CLI reads it.

Regenerate it in scratch with the supplied binary, optionally capturing one
current/baseline elapsed-time and checkpoint-size sample:

```sh
python3 tests/fixtures/graph-cutover/generate-terminal.py \
  --baseline /path/to/pre-cut/provenant \
  --current /path/to/current/provenant \
  --git /path/to/git --output "$TMPDIR/graph-cutover"
```

The script uses isolated HOME, state and Git configuration and no accounts.
The measurements describe a deterministic gate, excluding provider startup;
they are observations with no performance threshold. Release-host launches and
retained production-checkpoint inspection remain release checks.
