# Spec: graph runner for contract-driven plan graphs

Status: **proposed**, 2026-10-22. Implements requirement R2 of the
[operating model](operating-model.md) and is brought forward from its last
rollout position at the user's direction. GitHub owns delivery state; this
file owns the durable requirements. It grants no authority beyond what the user
approves by accepting it, and it does not relax any gate in `HARNESS.md`.

## Outcome

The chair states a piece of work as a graph (usually one line naming a
template) and the harness runs it across providers without a model waiting on
it. A deterministic process schedules nodes, enforces dependencies, runs gates,
repairs once, escalates once per tier, and reports to the chair only when
something needs a decision or the graph finishes. Every node leaves a receipt
the telemetry ledger can read.

Design rule: the runner schedules and records; it owns no provider mechanics,
no lifecycle state and no authority. Everything that starts a process goes
through the existing dispatch owner.

## Reuse map

| Need | Existing owner reused | What the runner adds |
|---|---|---|
| Start a provider process | `dispatchConfiguredProvider` (`execution.ts`), which normalises the route, applies workspace boundaries and runs `dispatch_run.py` | Calls it in-process, once per node attempt, with a deterministic `task_id` |
| Observe, cancel, retry | `observeOwner`, `cancelConfiguredRun`, `run_controls.py`, `cancel.request` | Decides when; never signals a PID |
| Lane listing and waits | Run registry, `provenant lanes`, `lanes --wait` | Nothing: node attempts are ordinary lanes |
| Receipts and route health | `fabric_records.py` attempt receipts | One graph-node receipt that references them |
| Liveness and exit fence | [worker-liveness](../../../skills/orchestrate/references/worker-liveness.md) | Starts a replacement only after the owner's observed exit |
| Pool and council picks | `pools.ts` (`route`, `rotate`, `council`, `models`) | Calls the picker at expansion time and records the pick |
| Confidentiality, secrets, boxes | Dispatch flags `confidential`, `allow_secrets`; R9 boxes later | Inherits and narrows; never sets |
| Task rows, `depends_on` | Fabric store `tasks`, `task_dependencies` | Enforces `depends_on`; the store keeps it as metadata |
| Messages to the chair | Fabric `send` and inbox | Terse notices on decision points |
| Capacity | R3 ledger (when it exists), route health, `provenant quota` | Reads a `capacity(adapter, account)` function with a static fallback |

New code is confined to `runtime/fabric/src/graph/` (schema and validation,
template expansion, scheduler, gate runner, receipts), three additive tables in
`runtime/fabric/schema.sql`, one `graph` command in `cli.ts`, one MCP tool and
one delegate row in `scripts/provenant`. No new adapter, daemon or Python
owner.

## Graph document

JSON, `schema_version: 1`, stored verbatim per revision. All expansion happens
before storage, so the runner only ever sees plain nodes.

```json
{"schema_version": 1, "graph_id": "g-7f3a21", "revision": 1,
 "origin": {"template": "fanout", "params": {"n": 5}, "request": "survey adapter retries"},
 "limits": {"max_width": 6, "max_nodes": 64, "max_depth": 3},
 "authority": {"mode": "read_only", "network": true, "confidential": false},
 "worktrees": {"w1": {"path": "/abs/path/.worktrees/x"}}, "nodes": []}
```

### Node fields

| Field | Type and rule |
|---|---|
| `id` | Unique within the graph, `[a-z0-9][a-z0-9._-]{0,47}`; sub-graph nodes are prefixed `<lead>/` |
| `kind` | `work` (default), `review`, `lead` |
| `role` and `tier` | Role from the R1.5 set (`scout`, `implementer`, `reviewer`, `lander`, `lead`); `tier` is `junior`, `senior` or `principal` |
| `route` | Same fields as `fabric_dispatch`: `adapter`, `alias` or `model`, `effort`, or a pool selector. Optional; absent means the tier's default from the size table (R1.1) |
| `execution` | Absent (a Fabric lane) or `headless` (Claude only; see Headless Claude) |
| `authority` | `mode` (`read_only` or `worktree_write`), `network`, `confidential`, `allow_secrets`, `capabilities`, `read_roots`. Narrowing only |
| `worktree` | Key into `worktrees`; required for `worktree_write` |
| `brief` | `prompt_file` path, or inline `prompt` the runner writes to the node's run directory |
| `inputs` | Parent outputs to attach: `[{from: "id", part: "result"\|"diff"\|"notes"}]`. Paths, never contents |
| `depends_on` | Node ids. A review node's `depends_on` is its children |
| `needs` | `passed` (default) or `fast`: the state of each parent that releases this node (see Gates) |
| `gate` | `{fast: [argv...], slow: [argv...], timeout_seconds}`; each entry is an argv array, never a shell string |
| `on_fail` | `repair` (default), `escalate`, `stop` |
| `context_budget` | Optional; tokens or percent of the route's context ceiling (R2.10) |
| `ui` | Optional `{viewports: [...], url or command}`; enables screenshots (R2.9) |
| `size`, `risk` | Recorded for telemetry and template defaults |

A node's spec hash covers every field above. Revisions compare hashes.

### Templates (R2.8)

A template is a pure function `(params, defaults) -> nodes`, run when the
request is made. The stored document contains the expanded nodes, the template
name, the params and the picks, so a resumed or revised graph never re-rolls a
pool choice unless asked (`--repick`).

| Template | Expansion |
|---|---|
| `single` | `work` writer, fast gates, one `review` node (other family), slow gates in parallel with the review |
| `fanout` | N `work` nodes (junior, read-only by default) over `items`, then one senior `work` merge node depending on all |
| `council` | N `review`-style lens nodes across families (pool picks recorded), then one principal synthesis node |
| `tree` | One `lead` node that emits a sub-graph, plus a `review` node over whatever it emits |
| `pipeline` | Items by stages: a chain of nodes per item, edges only within an item; no cross-item edge, so no barrier |

Size class and risk tier select the template defaults (width, tiers, routes,
gates, lenses, `on_fail`) from the policy file R1.1 owns; until it exists, from
`config/graph-defaults.json` seeded from the R1.2 table, which R1.1 then
absorbs.

**Overrides.** A request, or a saved template, may carry `overrides`: a map of
JSON-pointer-like paths (`defaults.tier`, `nodes.merge.route.adapter`,
`nodes.*.gate.fast`) to values. Overrides apply after expansion and before
validation, so an override can never skip a rule.

Templates nest (a node may carry `template: {...}`); expansion is flat and
bounded by `max_nodes` and `max_depth`.

**Saved templates.** `provenant graph templates save NAME --from GRAPH_ID
[--param key]` writes the expanded document with the named literals replaced by
`${key}` into the instance root (`<instance-root>/graph-templates/NAME.json`,
instance-owned per
[ADR 0019](../../adr/0019-installed-file-class-ownership.md)). A saved
template may declare `extends: "<built-in>"` plus overrides. Saved templates
never carry a `route` more permissive than their `authority` allows, and they
are re-validated on every use.

### Validation

Validation runs on submit, on revise and on every sub-graph splice. A failure
rejects the whole change with one line per fault and a `fix:`.

| Rule | Check |
|---|---|
| Acyclic | Kahn's algorithm over `depends_on`; unknown ids rejected |
| Depth | Chair-owned nodes are depth 1; each lead's sub-nodes add one; depth above 3 rejected |
| Size | `max_nodes` (default 64, ceiling 256) and `max_width` (default 6, ceiling 8, the batch ceiling) |
| Authority narrowing | For every child (by containment or by sub-graph): `mode` no higher, `network` no broader, `capabilities` and `read_roots` subsets, `allow_secrets` false unless the parent allowed it, `confidential` true if the parent was |
| Root authority | Root `authority` must not exceed the creating seat's own dispatch authority; checked by the same `workingIdentity` and boundary code dispatch uses |
| One writer per worktree | Writers sharing a `worktree` must be totally ordered by `depends_on` (a path exists between each pair); read-only nodes may share anything |
| Writer needs worktree | `worktree_write` without `worktree` rejected; the path must be a registered linked worktree, never the primary checkout |
| Review shape | A `review` node has at least one parent and `mode: read_only` |
| Route resolves | Every `route` passes `normaliseRoute` against the live catalogue now, so typos fail at submit, not mid-run |

## Runner

### Process model

`fabric graph run` validates, writes revision 1, then starts a detached
**graph owner**: a Node process (same session-leader pattern as `startOwner`)
running `fabric graph-owner GRAPH_ID`. The call returns at once with a digest.
The owner is plain code. It does not call a model, it holds no conversation
and the chair does not poll it: the chair gets a Fabric message when the graph
needs a decision or finishes, and may block with `provenant lanes --wait --all`
exactly as for any lane.

One owner per graph: a `graphs.owner` row (pid, start time, host) is claimed by
compare-and-set in a transaction. A second owner for the same graph exits.

### Store tables (additive, idempotent bootstrap)

| Table | Columns (abridged) | Role |
|---|---|---|
| `graphs` | `project, graph_id, revision, state, owner_pid, owner_started, chair_agent, created_at` | One row per graph; `state`: `running`, `done`, `stopped`, `cancelled`; `revision` is the current one |
| `graph_revisions` | `project, graph_id, revision, document_json, origin, created_at` | Immutable documents |
| `graph_nodes` | `project, graph_id, node_id, spec_hash, state, tier, attempt, revision, run_id, task_id, fast_ok, slow_ok, updated_at` | The only mutable scheduler state |

Node `state`: `pending`, `running`, `gating`, `fast_passed`, `passed`,
`failed`, `blocked` (worker question), `cancelling`, `cancelled`,
`superseded`, `stopped`. Every transition is one transaction guarded by
`WHERE revision = ? AND attempt = ?`, and writes a line to the Fabric activity
log (`graph g-7f3a21 node impl.a attempt 2 gating`), so `fabric_events` and the
console already see it. Each node is also a Fabric task row (`g-7f3a21.impl.a`)
with its `depends_on` filled in, so the existing task views show the graph
without a new reader.

Per-attempt files live under the project run root:
`.agent-run/graphs/<graph_id>/r<rev>/<node>/a<attempt>/{brief.md,receipt.json,gates/,notes.md}`.
The lane itself is a normal dispatch run directory, referenced by `run_id`.

### Scheduling

The owner loop is: read state, compute the **ready set**, start what capacity
allows, then sleep until a file changes.

1. **Ready set.** A `pending` node is ready when every parent is in the state
   its `needs` names (`passed`, or `fast_passed` or better) and its
   worktree, if any, has no running writer.
2. **Caps.** Start at most `min(graph max_width, free slots)`, where a slot is
   free when running nodes on the same `(adapter, account)` are below
   `cap(adapter, account)`. `cap` is the R3 ledger's live capacity for that
   pool, bounded by a static ceiling per adapter in `config/graph-defaults.json`
   (initially: OpenCode free 2 per model, Cursor 1, agy 2, Codex and Claude 3).
   A pool in cooldown contributes zero. Until the ledger exists the ceiling
   alone applies and route health supplies cooldowns.
3. **Order.** Critical path first (longest remaining chain), then cheaper tier
   first, then node id, so runs are reproducible.
4. **Rate limits are not failures.** A `rate_limited` or `usage_limited`
   dispatch outcome returns the node to `pending` with a retry time and does not
   consume repair or escalation budget (R3.3, R7.3). Route fallback is
   dispatch's existing behaviour.
5. **Pipeline.** Because each node is dispatched on its own, an item's next
   stage starts the moment its own previous stage passes; nothing waits for the
   batch. `fabric_batch` is not used by the runner.
6. **Wake-up.** The owner waits on file changes in the run directories of its
   running nodes (the `lane-watch` mechanism of
   [#1085](https://github.com/mblauberg/provenant/pull/1085) where merged; a
   two-second stat of `dispatch-status.json` files before then), with a
   30-second timer as the fallback for deaths no file records. No tokens are
   spent while waiting.

### Node attempt lifecycle

```text
pending -> running (dispatch, task_id g.<graph>.<node>.r<rev>.a<attempt>)
        -> owner exit observed -> receipt validated
        -> gating: fast gates (runner executes argv in the worktree)
              fail -> failed (repair/escalate ladder)
              pass -> fast_passed
        -> slow gates (and the dependent review node) run concurrently
              all pass -> passed
```

The runner executes gates as child processes with a timeout, an empty
inherited environment plus a short allowlist, the node's worktree as cwd, and
output sent to `gates/` files. A gate cannot widen authority: it is a command
the chair already put in the approved graph, run with the node's own box and
confidentiality settings.

### Crash and resume

State is the store plus the run directories, so the owner is restartable:

- Before dispatching, the owner writes the attempt row (`running`, `task_id`
  deterministic). On restart it looks the `task_id` up in the run registry. If
  the lane exists it is **adopted** (observe it); if not it is dispatched.
  Dispatch is therefore idempotent per `(graph, node, revision, attempt)`.
- Provider owners are detached session leaders and survive the graph owner.
- A dead owner is detected, not supervised: `graph status` and `lanes --wait`
  see a stale `owner_pid` plus unfinished nodes and print the exact
  `provenant graph resume GRAPH_ID`; the MCP tool resumes automatically on
  the next `status`. There is no watchdog daemon.
- A restart repeats no passed work and never starts a second writer in a
  worktree: the worktree lease below survives the restart.

### Worktree ownership

Static validation orders writers; a lease enforces it across graphs. A
`graph_worktree_leases(path, graph_id, node_id, attempt)` row (realpath key) is
taken in the transaction that marks a writer `running` and released only after
that attempt's owner exit is observed and its gates are done. Another graph
needing the path waits `pending`, reported as blocked by the holder. Dispatch's
concurrent-writer rejection remains the backstop.

## Revisions and stale results (R2.3)

`graph revise GRAPH_ID --file doc.json` (or the MCP `revise` action) submits a
new full document. The runner computes, per node id:

| Case | Action |
|---|---|
| Unchanged hash, parents unchanged | Keep state and result |
| Changed or removed, not started | Replace in place |
| Changed or removed, running | Request cancel through the existing control, mark `cancelling`; when the owner's exit is observed, mark `superseded`; only then start the replacement as a new attempt under the new revision |
| Passed but a parent changed | Reset to `pending`; keep the old result as read-only history |
| Added | `pending` |

Attempts record `(revision, attempt)` in their `task_id` and in every receipt.
When a result arrives, the runner accepts it only if the node row still has
exactly that `(revision, attempt)` and state `running` or `cancelling`; any
other result is written to the receipt as `stale` and its output is never
attached to a downstream brief. The conditional update makes this safe against
a revise that races a completion.

`graph cancel GRAPH_ID [--node ID]` follows the same fence: the node is
`cancelling` until the owner has exited, then `cancelled`. Reroute is a revise
that changes `route`. In both, no replacement or reuse of the worktree starts
before the exit is observed.

## Repair, escalation and stop (R2.5)

Per node, the ladder is fixed and bounded:

| Step | Trigger | Next |
|---|---|---|
| Attempt 1 at tier T | Fast gate, review or slow gate fails | One **repair** attempt at T, resumed in the same provider session where the adapter supports it, with the failure evidence attached as paths |
| Repair at T fails | | **Escalate**: new attempt at tier T+1 (junior to senior to principal) with brief, contract, failure evidence and the worktree as left; it also gets one repair |
| Repair at principal fails | | `stopped`; descendants become `blocked`; the chair is notified with the evidence paths |

`on_fail: escalate` skips the repair at the current tier; `on_fail: stop` stops
on first failure. Each escalation records `escalated_from`; each repair records
`repair_of`. Restarts for context (below) and rate-limit retries do not consume
the ladder. A writer's worktree is never reset by the runner; each attempt
records `base_sha` and `head_sha` so a later reviewer sees exactly the delta.
Counters are columns, not prompt text.

## Gates and review ordering (R2.9)

| Gate class | When it runs | Effect |
|---|---|---|
| Fast (lint, types, targeted tests) | After the writer exits, before review | Failure repairs without spending a review |
| Slow (full suites, CI) | In parallel with review, after fast passes | Failure joins review findings in one repair round |

A writer node reaches `fast_passed` after its fast gates and `passed` after its
slow gates too. A `review` node depends on its children with `needs: fast`, so
it starts while slow gates run. When both finish, their findings (gate output
paths plus the review result) feed **one** repair attempt on the writer; the
re-review brief carries only `git diff <pre_repair_sha>..HEAD`, plus the open
findings. A node is `passed` only when its fast and slow gates and any review
verdict pass, so every gate still passes before merge. CI as a slow gate is an
argv that waits on the pull request's checks (for example `gh pr checks
--watch`); it runs in the runner, not in a model.

**UI nodes.** If `ui` is set, after the fast gates the runner runs a capture
command (Playwright at each set viewport, written to `gates/screenshots/`) and
adds the image paths to the review brief's `inputs`. A reviewer route without vision
gets a note that images exist and are omitted; that review is not UI-complete. Capture failure is
a fast-gate failure.

## Review nodes over children (R2.4)

A `review` node is a read-only dispatch, by default a different family from its
children, whose brief carries its own contract, each child's result path and
`base_sha..head_sha` diff path, and gate results. It starts when every child is
`fast_passed`. Its result envelope classes findings `blocking` or `note`; a
`blocking` finding fails the children it names (all, if none) into the repair
ladder, and the review reruns on the delta.

## Sub-graphs from lead nodes (R2.2)

A `lead` node's result may end with a fragment envelope, parsed like the worker
terminal protocol (one exact JSON object, bounded to 64 KiB, malformed fails
closed as `fragment_invalid`):

```json
{"record_type":"provenant-graph-fragment","schema_version":1,
 "nodes":[{"id":"a","tier":"junior","depends_on":[],"...":"..."}]}
```

The runner prefixes ids with `<lead>/`, runs full validation against the lead's
authority and depth (the lead's depth plus one), and splices the fragment as a
new revision with `origin: lead:<id>`. The lead becomes `expanded`; its own
dependants wait on every sub-node (and the lead's `review` node, if any). An
invalid fragment is a failed attempt of the lead (one repair, as above), never a
partial splice. Total size is capped by `max_nodes`, so adaptive decomposition
cannot run away.

## Headless Claude nodes (R2.6)

Today `normaliseRoute` throws `native_seat_model` when a Claude seat dispatches
Claude through Fabric (`execution-input.ts:372-373`). The exception is exactly
one enumerated selector:

- `RouteInput.execution?: "headless"`. Any other value is an input error.
  Without it the refusal is unchanged.
- With it, on a Claude adapter, the guard is skipped and the route warns
  `HEADLESS: detached claude -p lane`. The existing Claude adapter already runs
  the CLI non-interactively with `--session-id` and `--resume`
  (`adapters/claude.py`).
- The selector is accepted from the graph runner and from `fabric_dispatch`
  (so the exception is testable and useful alone), but never by implication
  from `adapter: claude`.
- The provider session id from the attempt receipt is shown in `provenant lanes`
  (new `session` column and a `session_id` field in `--json`), with
  `claude --resume <id>` as the hint for opening a live or finished node.

Tests: the refusal without the selector (regression); an eligible headless
dispatch (non-interactive argv, session id); a mixed batch and graph (headless
Claude, Codex, OpenCode); rejection of any other `execution` value; the session
id in the lane row; `confidential` and `allow_secrets` passed through unchanged.

## Context budget and restarts (R2.10)

A node with `context_budget` (default: the route's `context_ceiling`) gets a
`notes.md` path in its brief and a standing instruction: one logical unit, one
commit; append a line to the notes file after each unit; end with a
`continue` classification when about 80% of the budget is used. The runner also
reads the context marker `context_usage.py` already produces. On a `continue`
(or an adapter outcome showing the ceiling reached), the owner observes exit and
starts a **fresh session** at the same tier with the original brief, the
contract, the notes file and `git log base_sha..HEAD`. A restart increments
`restart` (default maximum 3), not `attempt`, and does not consume the repair
ladder; exceeding the maximum is a failed attempt. The worker terminal protocol
gains the `continue` classification alongside `question`; it is parsed with the
same bounds.

## Surface

### CLI

| Verb | Effect |
|---|---|
| `provenant graph run TEMPLATE "request" [--size S] [--risk R] [--n N] [--items F] [--set path=value]... [--file doc.json]` | Expand, validate, store, start the owner; print the digest |
| `provenant graph status [ID] [--json] [--wait [--timeout N]]` | Digest of one graph, or the list; `--wait` blocks like `lanes --wait` |
| `provenant graph revise ID --file doc.json` | Publish a new revision |
| `provenant graph cancel ID [--node N]` | Fenced cancel |
| `provenant graph resume ID` | Restart a dead owner |
| `provenant graph templates [list\|show NAME\|save NAME --from ID [--param K]]` | Built-in and saved templates |

One-line requests:

```text
provenant graph run fanout "survey how each adapter retries" --n 5 --size S
provenant graph run council "should we adopt Z" --risk crucial
provenant graph run pipeline "migrate each page" --items pages.txt --set nodes.*.gate.fast='["pnpm","lint"]'
```

`graph` is a plain delegate in `scripts/provenant`
(`graph ... -> runtime/fabric/bin/fabric graph ...`), identical in shape to the
`lanes` and `events` rows, with its usage line added. The Rust front door being
built on `feat/rust-front-door` ports that dispatcher table; because `graph`
holds no logic outside the Fabric command, both front doors carry it by adding
one row, and `fabric graph` works unchanged underneath either.

### MCP

One tool, `fabric_graph`, with `action: run|status|revise|cancel|resume|templates`,
strict schema, the same compact-digest and `Route:` conventions as the dispatch
tools, and `wait_seconds` capped at 55 on `status`. It delegates to the same
functions as the CLI. This amends
[ADR 0022](../../adr/0022-thin-fabric-mcp-execution-facade.md), which fixes the
execution tools at two; slice 1 carries the amendment.

### Output

`run` and `status` return a digest of at most about 25 lines (graph id,
revision, state, passed count, then one line per node that is running,
repairing, blocked or stopped, with route, attempt and failing gate) and the
path of `graph.md`. Full state, gate output and receipts stay in run files;
`--json` and `detail: full` return `fabric.graph.v1` rows, the fields the
console reads. Chair notices are one-line Fabric messages: `stopped`,
`blocked` (with the exact resume call), `escalated to principal`, `done`,
`owner died`.

## Receipts and telemetry

Each attempt writes `receipt.json` (`schema: provenant.graph-node-receipt.v1`)
before the node leaves `running`:

| Group | Fields |
|---|---|
| Identity | `graph_id`, `revision`, `node_id`, `attempt`, `restart`, `depth`, `parent_ids`, `lead` |
| Plan | `template`, `kind`, `role`, `tier`, `size`, `risk`, `on_fail`, `repair_of`, `escalated_from`, `spec_hash` |
| Route | `adapter`, `model`, `effort`, `account` (null until R3), `execution`, `session_id`, `pick_reason`, `confidential` |
| Run | `run_id`, `task_id`, `status`, `queued_seconds`, `wall_seconds`, `base_sha`, `head_sha`, `stale` |
| Tokens | `input`, `cached_input`, `cache_write`, `output`, `reasoning`, `source` (`observed`, `estimated`, `null`), copied from the dispatch attempt receipt |
| Gates | `[{name, class: fast\|slow, argv_hash, exit, seconds, output_path}]`, review verdict, screenshot paths |

The graph receipt references the dispatch attempt receipt by `run_id` and
`task_id` and does not copy provider output (R4 deduplicates on those keys). R4.1
ingests `graph_nodes` and receipts keyed by `(graph_id, node_id, attempt)` and
joins them to Fabric attempt receipts, which gives cost per accepted task,
first-pass gate rate, escalations and graph efficiency (R4.4). Quota deltas
around a Claude node stay account-level aggregates (R4.5).

## Security and authority

- The graph owner holds no authority. It dispatches with the creating seat's
  `Identity`, stored on the `graphs` row, so every node is subject to the same
  workspace boundaries ([ADR 0021](../../adr/0021-configured-workspace-dispatch-boundaries.md)),
  capability rules and own-seat guard as a hand-made dispatch.
- Validation narrows and dispatch enforces; either failing rejects.
  A revision or fragment can only narrow relative to the root authority.
- `confidential` is sticky downward: a node under a confidential parent is
  confidential, so only non-training routes (D11) are eligible, and a
  confidential graph never routes to a free model that trains. Fallback and
  rotation inside dispatch keep the flag. `allow_secrets` is false unless the
  root set it and each ancestor allowed it.
- Writers need a registered linked worktree, never the primary checkout, and
  one writer per worktree holds throughout (lease above). Pushes to shared
  branches, publishing and posting are not node capabilities; they remain the
  chair's gated actions.
- Gate argv is fixed by the approved document; the runner never composes a
  shell string and never takes a command from a worker's output. A fragment
  from a lead can name gate commands only within the allowlist the root graph
  declares (`limits.gate_allowlist`, defaulting to the root's own gates).
- R9 boxes apply per node when they ship; a node whose adapter lacks a passing
  box falls back to today's restrictions, as R9.5 requires.

## Slice plan

Each slice is an independent pull request with its own tests, behind the same
`graph` surface. Slice 1 is usable alone for a real mixed-provider fan-out.

| # | Slice | Delivers | R2 acceptance covered |
|---|---|---|---|
| 1 | Static runner | Schema v1 (plain nodes), validation (acyclic, depth, narrowing, one writer), owner process, ready-set scheduling with `depends_on` enforcement and static caps, fast gates, per-node receipt, `graph run/status/cancel/resume`, MCP `fabric_graph`, delegate row, ADR 0022 amendment | Three-tier mixed-provider graph with no idle coordinator; every node writes a receipt; cancel exits before reuse |
| 2 | Repair, escalation, review | Attempt ladder (repair once, escalate, stop), `on_fail`, `review` nodes, slow gates in parallel with review, one repair round, delta re-review, chair notices | Failing leaf repairs once then escalates; review over children; gate ordering |
| 3 | Revisions | `graph revise`, hash diff, fenced supersede, stale-result rejection, worktree lease across graphs | Mid-run revision with no stale result accepted; replacement after exit |
| 4 | Templates | The five built-ins, size/risk defaults, overrides, nesting, saved templates, one-line `graph run TEMPLATE`, pool picks recorded, `templates` verb | Template graph from a one-line request and from a node-level override |
| 5 | Sub-graphs and headless Claude | `lead` nodes and fragment splice, `execution: headless` with its tests, session ids in `lanes` | Adaptive decomposition; headless nodes in a mixed graph; R2.6 tests |
| 6 | Context, UI and ledger | `context_budget`, notes file, `continue` restarts, UI screenshots, capacity from the R3 ledger when present, receipts exposed to R4 ingestion | R2.9 UI clause; R2.10 |

R2.7 is not a slice: see Status of R2.7.

Slices 2 and 3 make runs trustworthy under failure and change; slice 5
widens what a node may do, so it follows; slice 6 depends on R3 and R4 and
degrades gracefully without them.

## Status of R2.7

Partly fixed on main. `startOwner` writes a lane pointer into the dispatch
`cwd` project (`writeLanePointers`, from d1412ec2 and a82b82c8), and dispatch
sets `cwdProjects` from the worktree or `cwd`, so an explicit `cwd` or
`worktree` already registers the lane under that project. The remaining case,
observed on 2026-10-21, is an MCP host that kept the chair's start directory
while the chair's shell sits in a repository; the lane's source project is
then the host's, and `lanes --wait` from the repository finds no pointer for
a dispatch that named neither `cwd` nor `worktree`. Open pull request
[#1085](https://github.com/mblauberg/provenant/pull/1085) closes it by reading
lanes launched under the same seat's process ancestry, which is a different
mechanism from R2.7's wording. Graph nodes always name `cwd` or `worktree`, so
the runner does not depend on #1085; slice 1 adds a test that a node dispatched
into a repository is visible to `lanes --wait` run from that repository.

## Non-goals

- No model coordinator, no polling by the chair, no watchdog daemon.
- No new provider adapter, lifecycle engine or acceptance authority. A graph
  `done` is an execution outcome, not acceptance (R4.3); landing and review
  gates in `HARNESS.md` apply as before.
- No cross-host graph scheduling in v1; a node runs on the owner's host, and
  federation ([fabric-hosts](../fabric-hosts.md)) can carry nodes later by
  reusing its dispatch path.
- No concurrent writers in one worktree; no automatic worktree creation in
  slices 1 to 3 (nodes name existing worktrees; a helper that calls the
  existing worktree command follows).

## Open questions

| # | Question | Recommendation |
|---|---|---|
| 1 | Should the runner create linked worktrees for writer fanouts, or must the chair supply them? | The chair supplies them for slices 1 to 3. Then add `worktrees: auto` calling the existing worktree command, since fanouts of writers are where users will want it. |
| 2 | Should slow gates that are CI checks block `passed`, given CI can take many minutes? | Yes. `passed` means every gate passed; downstream nodes with `needs: fast` already proceed, and the chair is not waiting, since the runner notifies. |
| 3 | Where do saved templates live: the instance root, or the repository? | Instance root by default (personal, instance-owned); a repository may check in its own under `config/graph-templates/`, which wins for that project. |
