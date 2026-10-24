# Spec: graph runner for contract-driven plan graphs

Status: **proposed**, 2026-10-22. Implements requirement R2 of the
[operating model](operating-model.md) and is brought forward from its last
rollout position at the user's direction. GitHub owns delivery state; this
file owns the durable requirements. It grants no authority beyond what the user
approves by accepting it, and it does not relax any gate in `HARNESS.md`.

## Outcome

The chair states a piece of work as a graph and the harness runs it across
providers without a model waiting on it. A deterministic process schedules
nodes, enforces dependencies, runs gates, and reports to the chair only when
something needs a decision or the graph finishes. Every node attempt leaves a
receipt the telemetry ledger can read.

Design rule: the runner schedules and records; it owns no provider mechanics,
no lifecycle state and no authority. Everything that starts a provider process
goes through the existing dispatch owner.

## Reuse map

| Need | Existing owner reused | What the runner adds |
|---|---|---|
| Start a provider process | `dispatchConfiguredProvider` (`execution.ts`): route normalisation, workspace boundaries, `dispatch_run.py` | Calls it in-process, once per attempt, with a hashed `task_id` and an `onLaunch` observer |
| Owner survival | `hostOwnersInThemselves()` in `execution.ts`, as the CLI already does | Called at graph-owner start, so `reapOrphanedRuns` in `process-identity.ts` never stops a lane because the graph owner died |
| Observe, cancel | Run registry liveness (`findRecordedRun`, `runProcessAlive`, `processMatches`), `cancelConfiguredRun`, `cancel.request` | Decides when. Never uses `observeOwner`, which needs the in-process child handle |
| Lane listing and waits | Run registry, `provenant lanes`, `lanes --wait` | Nothing: attempts are ordinary lanes |
| Artefact retention | `pruneDispatchRuns` in `run-registry.ts` skips a run directory holding `KEEP` | Pins every referenced run until the graph is terminal |
| Receipts and route health | `fabric_records.py` attempt receipts | One graph-node receipt that references them |
| Liveness and exit fence | [worker-liveness](../../../skills/orchestrate/references/worker-liveness.md) | Extends the fence to gate processes |
| Worktree writer lease | `acquire_worktree_lease` (`dispatch_run.py:900`) | A reservation spanning the attempt and its gates, which dispatch also honours |
| Confidentiality, secrets | Dispatch flags `confidential`, `allow_secrets` | Propagates and narrows; never widens |
| Task rows, `depends_on` | Fabric store `tasks`, `task_dependencies` | Enforces `depends_on`; the store keeps it as metadata |
| Messages to the chair | Fabric `send` and inbox | Terse notices on decision points |
| Capacity | Static ceilings now; R3 ledger later | Transactional slot reservation |

New code is confined to `runtime/fabric/src/graph/` (schema, validation,
scheduler, gate runner, receipts), additive tables in
`runtime/fabric/schema.sql`, one `graph` command in `cli.ts`, one MCP tool, one
delegate row in the Rust front door, and a reservation check in
`dispatch_run.py`. No new adapter, daemon or Python owner.

## Graph document

JSON, `schema_version: 1`, stored verbatim per revision. All expansion happens
before storage, so the runner only ever sees plain nodes.

```json
{"schema_version": 1, "graph_id": "g-7f3a21", "revision": 1,
 "origin": {"request": "survey adapter retries"},
 "limits": {"max_width": 6, "max_nodes": 64, "max_depth": 3,
            "gate_allowlist": [["pnpm", "lint"], ["pnpm", "test", "--filter", "fabric"]]},
 "authority": {"mode": "read_only", "network": true, "confidential": false},
 "worktrees": {"w1": {"path": "/abs/path/.worktrees/x"}}, "nodes": []}
```

`graph_id` matches `g-[0-9a-f]{6,16}`.

### Node fields

| Field | Type and rule |
|---|---|
| `id` | Unique within the graph, `[a-z0-9][a-z0-9._-]{0,47}`; sub-graph nodes are prefixed `<lead>/`. Never used directly as a dispatch id |
| `kind` | `work` (default), `review`, `lead` |
| `role` and `tier` | Role from the R1.5 set; `tier` is `junior`, `senior` or `principal` |
| `route` | Same fields as `fabric_submit`. Required in slice 1; later optional, defaulting from the size table (R1.1) |
| `execution` | Absent (a Fabric lane) or `headless` (Claude only; see Headless Claude) |
| `authority` | `mode` (`read_only` or `worktree_write`), `network`, `confidential`, `allow_secrets`, `capabilities`, `read_roots`. Narrowing only |
| `worktree` | Key into `worktrees`; required for `worktree_write` |
| `brief` | `prompt_file` path, or inline `prompt` the runner writes to the attempt directory |
| `inputs` | Parent outputs to attach: `[{from: "id", part: "result"\|"diff"\|"notes"}]`. Paths, never contents |
| `depends_on` | Node ids. A review node's `depends_on` is its children. Every `inputs.from` must also appear here |
| `needs` | `passed` (default) or `fast` (slice 2): the parent state that releases this node |
| `gate` | `{fast: [argv...], slow: [argv...], timeout_seconds}`; argv arrays only, each exactly matching an entry of `limits.gate_allowlist` |
| `on_fail` | `stop` in slice 1; `repair` (default from slice 2) or `escalate` |
| `context_budget`, `ui` | Slice 6 (R2.10, R2.9) |
| `size`, `risk` | Recorded for telemetry and template defaults |

A node's spec hash covers its **effective** execution spec: the fields above
after defaults and pool picks, the resolved worktree realpath, the effective
(inherited and narrowed) authority, the brief's content hash and the gate argv.
Revisions compare hashes, so changing `worktrees.w1` or root authority
invalidates the writer.

### Validation

Validation runs on submit, on revise and on every sub-graph splice. A failure
rejects the whole change with one line per fault and a `fix:`.

| Rule | Check |
|---|---|
| Acyclic | Kahn's algorithm over `depends_on`; unknown ids rejected |
| Depth | Chair-owned nodes are depth 1; each lead's sub-nodes add one; depth above 3 rejected |
| Size | `max_nodes` (default 64, ceiling 256) and `max_width` (default 6, ceiling 8, the batch ceiling) |
| Authority narrowing | For every child (by containment or sub-graph): `mode` no higher, `network` no broader, `capabilities` and `read_roots` subsets, `allow_secrets` false unless the parent allowed it |
| Confidentiality closure | See Security. A node that consumes any confidential artefact is confidential; an explicit `confidential: false` on such a node is rejected (`confidentiality_downgrade`) |
| Root authority | Root `authority` must not exceed the creating seat's own dispatch authority; checked by the same `workingIdentity` and boundary code dispatch uses |
| One writer per worktree | Writers sharing a `worktree` must be totally ordered by `depends_on`; read-only nodes may share anything |
| Writer needs worktree | `worktree_write` without `worktree` rejected; the path must be a registered linked worktree, never the primary checkout |
| Review shape | A `review` node has at least one parent and `mode: read_only` |
| Gate allowlist | Every gate argv is in `limits.gate_allowlist`, which the chair authorises with the graph |
| Route resolves | Every `route` passes `normaliseRoute` with the same own-seat guard dispatch would apply, and must name a catalogue entry: the unknown-selector pass-through (`execution-input.ts:368`) is an error here, not a warning. This proves the route is well formed, not that it is executable now; auth, quota and cooldown are checked by dispatch per attempt |

## Runner

### Process model

`fabric graph run` validates, writes revision 1, then starts a detached
**graph owner**: a Node process (same session-leader pattern as `startOwner`)
running `fabric graph-owner GRAPH_ID`. The call returns at once with a digest.
It calls no model and nobody polls it: the chair gets a Fabric message on a
decision point or completion, and may block with `provenant lanes --wait --all`.

It calls `hostOwnersInThemselves()` before its first dispatch, so provider
owners are their own hosts and survive the graph owner.

One owner per graph, claimed by compare-and-set on `graphs.owner_pid` and
`owner_started`; liveness is `processMatches`, so PID reuse never passes.

### Store tables (additive, idempotent bootstrap)

| Table | Columns (abridged) | Role |
|---|---|---|
| `graphs` | `project, graph_id, revision, state, owner_pid, owner_started, chair_identity, created_at` | `state`: `running`, `done`, `stopped`, `cancelled` |
| `graph_revisions` | `project, graph_id, revision, document_json, origin, created_at` | Immutable documents |
| `graph_attempts` | `project, graph_id, node_id, revision, attempt, restart, state, task_id, run_id, input_attempts, adapter, model, account, gate_procs, updated_at` | Mutable scheduler state, one row per attempt; the node's current attempt is the latest non-superseded row |
| `graph_worktree_reservations` | `realpath, graph_id, node_id, revision, attempt, token, created_at` | Primary key `realpath` |

Attempt `state`: `pending`, `running`, `gating`, `fast_passed`, `passed`,
`failed`, `blocked`, `cancelling`, `cancelled`, `superseded`, `stopped`.
Each transition is one transaction guarded by `(revision, attempt, state)` and
writes one activity line, so `fabric_events` and the console see it. Each node
is also a Fabric task row with `depends_on` filled in.

Per-attempt files: `.agent-run/graphs/<graph_id>/r<rev>/<node>/a<attempt>/{brief.md,receipt.json,gates/}`.
The lane itself is a normal dispatch run directory, referenced by `run_id`.

### Dispatch identity

`task_id` is `<graph_id>.` followed by the first 24 hex digits of SHA-256 over
canonical JSON `{project, graph_id, node_id, revision, attempt, restart}`. It
is at most 43 characters and matches dispatch's
`^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$` (`dispatch_run.py:43`) for any node id,
including sub-graph ids with `/`. A context restart is a new identity, so it
never adopts the previous session's lane.

The owner writes the attempt row as `pending` with its `task_id`, then
dispatches with an `onLaunch` observer that records `run_id` in that row before
the owner process starts. The observer throwing aborts the launch
(`LaunchObserver`, `execution.ts:349`), so no lane exists that the graph has
not recorded. In the same step the runner creates `KEEP` in the run directory
(exclusive create, so it removes only a pin it made) and keeps it until the
graph is terminal and its receipts are written.

### Scheduling

1. **Ready set.** A `pending` node is ready when every parent's current attempt
   is in the state its `needs` names and its worktree, if any, is unreserved or
   reserved by this node.
2. **Reservation.** An attempt is **active** while its provider owner or any
   recorded gate group lives, whatever its state (`fast_passed` included). In
   one `BEGIN IMMEDIATE` transaction the owner counts active attempts in this
   graph, and attempts holding a provider slot in each pool
   `(adapter, model, account)` across every graph, then moves a ready attempt to `running` only if graph occupancy is
   below `max_width` and pool occupancy is below `cap(adapter, model, account)`.
   The writer's worktree reservation joins the same transaction, so no wake or
   concurrent owner overfills a cap. An attempt holds a provider slot from the
   transaction that moves it to `running` until its launch fails or its provider
   owner exits, so a launch still in dispatch preflight counts; gate-only
   attempts hold none. `account` is `default` until R3. `cap` is the static ceiling in
   `config/graph-defaults.json` (initially OpenCode free 2 per model, Cursor 1,
   agy 2, Codex and Claude 3), bounded by the R3 ledger when it exists; a pool
   in cooldown contributes zero. Active hand-made registry lanes count against
   the pool, so caps are exact between graphs and best effort against hand
   dispatch.
3. **Order.** Critical path first, then cheaper tier, then node id.
4. **Rate limits preserve launch evidence.** A refusal before provider launch
   may try the next configured route. After launch, only a read-only node may
   fall back. A writer fails and reports its worktree for chair inspection.
5. **Pipeline.** Each node is dispatched on its own, so an item's next stage
   starts when its own previous stage passes. The graph owner submits the individual ready tasks.
6. **Wake-up.** File changes in running attempts' run directories (the
   `lane-watch` mechanism of
   [#1085](https://github.com/mblauberg/provenant/pull/1085) where merged, a
   two-second stat of `dispatch-status.json` before then), plus a 30-second
   timer for deaths no file records. No tokens are spent waiting.

### Gates

In slice 1 gates are **trusted commands** from the chair-authorised
`limits.gate_allowlist`, run with the user's filesystem and network reach. A
timeout, minimal environment, worktree cwd and `gates/` output files are not
confinement; none is claimed until R9. Workers never supply a gate command.

**Launch handshake.** The runner commits a gate intent (`gate_procs` entry with
a launch token) before spawning `fabric gate-exec TOKEN` as a new
process-group leader that blocks on a pipe from the owner. The owner commits
`{pid, pgid, started_at, argv_hash}`, then writes `go`; only then does the
wrapper run the argv as a child in its group. If the owner dies first, the
wrapper reads EOF and exits having run nothing, so no gate runs unrecorded. A
gate without a recorded exit status never counts as passed.

**Group termination** (slice-1 code requirement). `signalRecordedProcess` in
`process-identity.ts` signals only the PID, so the registry gains
`signalRecordedGroup`, which signals `-pgid` only while the group is
attributable: the leader matches its recorded start time, or the leader has
exited and no live process has pid equal to the pgid. SIGTERM, then SIGKILL
after a grace period. A gate group has exited when `kill(-pgid, 0)` returns `ESRCH`, or when the
pgid is positively owned by a different process, its start time differing from
the recorded one (the `pgid_gone` and `pgid_reused` cases `dispatch_run.py:626`
and `:636` already distinguish). An unknown or failed probe stays fenced. No
rerun or reservation release happens before one of those holds.

Slice-1 tests cover the handshake's edges: the owner is the only holder of the
pipe's write end (close-on-exec everywhere else), EOF before `go` runs nothing,
a buffered `go` written just before owner death still runs a recorded gate, and
the wrapper reaps the command so a zombie never reads as live.

A CI gate passes only on checks for the attempt's exact `head_sha` (compared
against the check suite's SHA), so another push cannot satisfy it.

### Worktree reservation

A reservation enforces one writer across graphs and against hand dispatch, from
the transaction that marks a writer `running` until the attempt is not active.

- The reservation is the store row plus a file in the worktree's Git directory
  beside dispatch's lock (`provenant-graph-reservation.json`: graph, node,
  attempt, token, owner identity), created exclusively.
- `dispatch_run.py` checks that file before `acquire_worktree_lease`. A live
  reservation whose token the dispatch does not carry is refused
  (`worktree_reserved`, naming the graph and node). The graph passes its token
  to its own dispatch. A reservation whose graph is terminal or whose row is
  gone is stale and removable.
- During gates the graph owner also holds dispatch's flock itself, so
  dispatch's concurrent-writer rejection stays the backstop.

### Crash and resume

- On restart each active or non-terminal attempt (`running`, `gating`,
  `fast_passed`, `cancelling`) is found by its recorded `run_id`. A live lane is adopted and observed through the registry; a
  finished lane's receipt is read. A run with no owner record and no receipt
  never started (the crash fell between `onLaunch` and `startOwner`) and is
  redispatched as a new attempt outside the ladder; a row with no `run_id` was
  never launched. Nothing is redispatched under an identity that has a lane.
- Each recorded gate group still alive is terminated with
  `signalRecordedGroup` and its exit verified; an intent with no pid never ran.
  Any gate without an exit status then reruns. Reservation is kept throughout.
- A dead owner is detected, not supervised: `graph status` and `lanes --wait`
  print the exact `provenant graph resume GRAPH_ID`; the MCP tool resumes on
  the next `status`. A restart repeats no passed work.

## Revisions and invalidation (R2.3)

`graph revise GRAPH_ID --file doc.json` submits a new full document. Per node:

| Case | Action |
|---|---|
| Unchanged hash, no consumed input invalidated | Keep state and result |
| Changed or removed, not started | Replace in place |
| Changed or removed, running or gating | Fenced cancel (below), mark `superseded` after exit, then start the replacement under the new revision |
| Added | `pending` |

**Input binding.** At dispatch the attempt records `input_attempts`: the
`(node_id, revision, attempt)` of every parent attempt whose output or state
released it, including repair and review evidence. A result is accepted only if
the attempt row still matches its `(revision, attempt)` and every recorded
input attempt is still that parent's current accepted attempt. Anything else is
written to the receipt as `stale` and never attached downstream.

**Transitive invalidation.** When a node's current accepted attempt changes
(revision, repair or escalation), every descendant whose `input_attempts`
reaches the old attempt, directly or transitively, is invalidated in the same
transaction. Each is marked `cancelling` and fenced (provider owner cancelled,
gate groups terminated); it returns to `pending` only after verified exit. A
`fast_passed` attempt with live slow gates is fenced like a running one. Old
results stay as history.
This covers an unchanged consumer that would otherwise finish on obsolete input,
and `needs: fast` consumers of a parent that is later repaired.

`graph cancel GRAPH_ID [--node ID]` uses the same fence: the provider owner is
cancelled through `cancelConfiguredRun`, recorded gate groups are terminated
through `signalRecordedGroup`, and the attempt stays
`cancelling` until all have exited. Reroute is a revise that changes `route`.
No replacement or worktree reuse starts before that exit; this holds across a
resume.

## Repair, escalation and stop (R2.5)

| Step | Trigger | Next |
|---|---|---|
| Attempt 1 at tier T | Fast gate, review or slow gate fails | One **repair** at T, resumed in the same provider session where supported, with failure evidence as paths |
| Repair at T fails | | **Escalate** to T+1 with brief, contract, evidence and the worktree as left; it also gets one repair |
| Repair at principal fails | | `stopped`; descendants `blocked`; chair notified with evidence paths |

`on_fail: escalate` skips the repair at the current tier; `stop` stops at once.
Attempts record `repair_of` or `escalated_from`, `base_sha` and `head_sha`.
The runner never resets a worktree.

## Gates and review ordering (R2.4, R2.9)

Fast gates (lint, types, targeted tests) run after the writer exits and before
review; slow gates (full suites, CI) run in parallel with review. A writer is
`fast_passed` after fast gates and `passed` only after fast gates, slow gates
and any review verdict pass, so a slow gate always blocks `passed`. A `review`
node depends on its children with `needs: fast`, is read-only, by default of
another family, and receives each child's result, `base_sha..head_sha` diff and
gate output paths. Its envelope classes findings `blocking` or `note`; blocking
findings and slow-gate failures feed **one** repair round on the named
children, and re-review receives only `git diff <pre_repair_sha>..HEAD` plus the
open findings.

## Sub-graphs from lead nodes (R2.2)

A `lead` node's result may end with one fragment envelope, parsed like the
worker terminal protocol (one exact JSON object, at most 64 KiB, malformed fails
closed as `fragment_invalid`):

```json
{"record_type":"provenant-graph-fragment","schema_version":1,
 "nodes":[{"id":"a","tier":"junior","depends_on":[],"...":"..."}]}
```

The runner prefixes ids with `<lead>/`, validates against the lead's authority
and depth plus one, and splices the fragment as a new revision with
`origin: lead:<id>`. A fragment may name gates only from the root's
`limits.gate_allowlist`. The lead's dependants wait on every sub-node. An
invalid fragment is a failed attempt of the lead, never a partial splice.

## Headless Claude nodes (R2.6)

`normaliseRoute` throws `native_seat_model` when a Claude seat names Claude
explicitly (`execution-input.ts:372`). The exception is one enumerated
selector, scoped to the route that carries it:

- `RouteInput.execution?: "headless"`; any other value is an input error.
  Without it the refusal is unchanged. `ownSeatGuard` is never disabled
  globally.
- The selector is carried, unchanged, through every normalisation a route
  passes: single dispatch (`execution.ts:805` and `:811`), batch preflight (the
  `OWN_SEAT_GUARD` marker at `:1010` and the initial normalise at `:946`), pool
  expansion and `applyDispatchDefaults`, and final normalisation (`:951`). Any
  path that drops it fails closed with the original refusal.
- With it, on the Claude adapter only, the guard is skipped with the warning
  `HEADLESS: detached claude -p lane`; `adapters/claude.py` already runs
  non-interactively with `--session-id` and `--resume`. Never implied by
  `adapter: claude`. The session id appears in `provenant lanes` (`session`,
  `session_id` in `--json`) with `claude --resume <id>` as the hint.

Tests: refusal without the selector; a headless dispatch; a mixed batch and
graph where the selector survives preflight and final normalisation while a
sibling Claude task without it is refused; other values rejected; session id
in the lane row; flags unchanged.

## Templates (R2.8)

Requirements; R2.8 holds the shapes.

- A template is a pure function `(params, defaults) -> nodes`, run at request
  time. The stored document holds the expanded nodes, params and pool picks, so
  nothing re-rolls a pick unless asked (`--repick`). Defaults come from R1.1,
  or `config/graph-defaults.json` until it exists.
- `overrides` (path to value, for example `nodes.merge.route.adapter`) apply
  after expansion and before validation, so no override skips a rule.
  Templates nest within `max_nodes` and `max_depth`.
- Saved templates live in the instance root
  (`<instance-root>/graph-templates/NAME.json`, instance-owned per
  [ADR 0019](../../adr/0019-installed-file-class-ownership.md)). A repository's
  `config/graph-templates/NAME.json` overrides the instance template of the same
  name for that project. `save` must parameterise machine-specific values
  (absolute paths, worktree paths, accounts) as `${key}`, and refuses a document
  that still contains them. Every use re-validates.

Acceptance: one-line request; node-level override; repository template wins;
save with an absolute path left in refused.

## Context budget and restarts (R2.10)

Requirements; R2.10 holds the policy.

- A node with `context_budget` gets a `notes.md` path and the standing
  one-unit, one-commit, append-a-note instruction.
- A `continue` classification in the worker terminal protocol, or an adapter
  outcome showing the ceiling reached, starts a fresh session at the same tier
  with brief, contract, notes and `git log base_sha..HEAD`. It increments
  `restart` (default maximum 3), which changes the `task_id`; it does not
  consume the ladder. Exceeding the maximum is a failed attempt.

Acceptance: two restarts then pass; exceeding the maximum enters the ladder.

## Surface

### CLI

| Verb | Effect |
|---|---|
| `provenant graph run [TEMPLATE "request" [--set path=value]...] [--file doc.json]` | Expand (slice 4), validate, store, start the owner; print the digest. Slice 1 takes `--file` only |
| `provenant graph status [ID] [--json] [--wait [--timeout N]]` | Digest of one graph or the list |
| `provenant graph revise ID --file doc.json` | New revision (slice 3) |
| `provenant graph cancel ID [--node N]` | Fenced cancel |
| `provenant graph resume ID` | Restart a dead owner |
| `provenant graph templates [list\|show NAME\|save NAME --from ID --param K...]` | Slice 4 |

Example: `provenant graph run fanout "survey how each adapter retries" --set n=5`.

`graph` is a plain delegate row in the Rust front door, like `lanes`, so the
Rust front door ports it as one row.

### MCP

Use `fabric_graph` with `op: run|status|cancel|resume`. Run and resume take a
project-local document path; status and cancel take `id`. Each call is bounded
by the MCP request budget of 55 seconds. The [Rust Fabric design](../rust-port/design/fabric.md)
owns the current checkpoint and execution contract.

### Output

`run` and `status` return a digest of about 25 lines at most (one line per
running, blocked or stopped node); `--json` and `detail: full` return
`fabric.graph.v1` rows. Chair notices are one-line messages: `stopped`,
`blocked` (with the resume call), `escalated`, `done`, `owner died`.

## Receipts

Each attempt writes `receipt.json` (`provenant.graph-node-receipt.v1`) holding
its identity (including `input_attempts`), plan, route, outcome (`stale`,
`base_sha`, `head_sha`, per-gate exit and output path) and token counts copied
from the dispatch receipt, which it references by `run_id` and `task_id`
without copying provider output. R4 keys on
`(graph_id, node_id, revision, attempt, restart)`.

Acceptance: every attempt in a mixed three-provider graph has a receipt that
joins to its dispatch receipt; a stale result's receipt is marked `stale`.

## Security and authority

- The graph owner holds no authority. It dispatches with the creating seat's
  `Identity`, stored on the `graphs` row, so every node meets the same
  workspace boundaries
  ([ADR 0021](../../adr/0021-configured-workspace-dispatch-boundaries.md)),
  capability rules and own-seat guard as a hand-made dispatch.
- **Confidentiality propagates through data, not only containment.** A node is
  confidential if the root is, if any containing ancestor is, or if it consumes
  any artefact of a confidential node: `inputs`, parent results, diffs, gate
  output, review findings, repair and escalation evidence, or a lead's fragment.
  The closure is computed at validation over the whole graph and again whenever
  the runner attaches evidence; an attachment that would reach a
  non-confidential attempt is refused (`confidentiality_downgrade`), never
  silently raised mid-run. Confidential nodes route only to non-training models
  (D11); dispatch fallback and rotation keep the flag.
- Pushes to shared branches, publishing and posting are not node
  capabilities; they stay the chair's gated actions.
- Gate argv comes only from the chair-authorised allowlist; the runner never
  composes a shell string or takes a command from worker output. Gates are
  unconfined until R9; a node whose adapter lacks a passing box keeps today's
  restrictions (R9.5).

## Slice plan

Each slice is an independent pull request with its own tests behind the same
`graph` surface.

| # | Slice | Delivers | Acceptance |
|---|---|---|---|
| 1 | Static runner | Expanded JSON work nodes, validation (including confidentiality closure and gate allowlist), effective spec hash, explicit routes, `depends_on` enforcement, transactional static caps, trusted fast gates with launch handshake and `signalRecordedGroup`, worktree reservation honoured by `dispatch_run.py`, hashed task ids, `onLaunch` recording and `KEEP` pins, input-attempt binding in receipts, crash recovery through the registry, `graph run --file/status/cancel/resume`, notices, MCP `fabric_graph`, delegate row, ADR 0022 amendment. Stop on failure | Three-tier mixed-provider graph with no idle coordinator; graph owner killed mid-run, lanes survive and resume adopts them; cancel during a gate whose command forks children leaves no group member before the reservation releases; owner killed between gate intent and `go` leaves no gate running; hand dispatch into a reserved worktree refused; two concurrent graphs never exceed a pool cap; confidential parent feeding a non-confidential synthesis node rejected; a node dispatched into a repository visible to `lanes --wait` from it |
| 2 | Repair, escalation, review | Ladder, `on_fail`, `needs: fast`, `review` nodes, slow gates with CI commit check, one repair round, delta re-review, invalidation on repair | Failing leaf repairs once then escalates; `fast` consumer of a repaired parent reruns; CI result for an older SHA does not pass |
| 3 | Revisions | `graph revise`, hash diff, fenced supersede, transitive invalidation | Mid-run revision: a running unchanged descendant of a changed node does not get its result accepted |
| 4 | Templates | Built-ins, defaults, overrides, nesting, saved and repository templates, `templates` verb | See Templates |
| 5 | Sub-graphs and headless Claude | `lead` nodes and splice, `execution: headless`, session ids in `lanes` | See Headless Claude |
| 6 | Context, UI and ledger | `context_budget` and restarts, UI screenshots (R2.9), R3 ledger capacity, R4 ingestion | See Context budget; R2.9 UI clause |

Writers ship in slice 1 because the reservation and fencing ship with it. If
any slice 1 fencing acceptance case is not met, slice 1 ships read-only and
writers wait for it.

**R2.7.** Graph nodes always name `cwd` or `worktree`, which main already
registers (`writeLanePointers`), so the runner does not depend on
[#1085](https://github.com/mblauberg/provenant/pull/1085).

## Non-goals

- No model coordinator, no polling by the chair, no watchdog daemon.
- No new provider adapter, lifecycle engine or acceptance authority. A graph
  `done` is an execution outcome, not acceptance (R4.3).
- No cross-host scheduling in v1; [fabric-hosts](../fabric-hosts.md) can
  carry nodes later.

## Decisions

Accepted by the user on 2026-10-22.

| # | Decision |
|---|---|
| 1 | The chair supplies writer worktrees first. `worktrees: auto`, calling the existing worktree command, follows once slices 1 to 3 are in. |
| 2 | Slow gates, CI included, block `passed`; `needs: fast` consumers proceed meanwhile. A CI gate verifies the exact expected commit. |
| 3 | Saved templates live in the instance root; a repository's `config/graph-templates/` overrides them for that project. Saved templates parameterise machine-specific paths. |
