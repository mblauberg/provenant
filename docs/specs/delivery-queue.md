# Durable delivery queue

Owner: graph execution, templates, run admission and landing
Anchor: `crates/fabric-runtime/src/graph/`, `src/fabric/interface.rs`,
`src/tick.rs`, `src/admit.rs`, `src/pr/landing/`

This is the target contract for [#3972](https://github.com/mblauberg/provenant/issues/3972),
not a claim that the interfaces below are implemented. Paths are relative to
`tools/front-door/` unless stated otherwise.

## Intent and boundaries

A queue delivers independent tracker items through ordinary graph instances.
No resident process or product REPL is needed: durable graph state, receipts and
inbox survive restarts; each scheduled or invoked `tick` advances due work. Tick
must not be assumed to run continuously. Existing authority, custody, scope and
landing policies still apply. Every policy below is sourced, inspectable and
overridable at the appropriate layer; thresholds warn before refusing unless a
real custody, authority, integrity or resource boundary requires refusal. One
item has one graph owner and at most one active writer; a retry transfers custody
only after the previous writer settles. No merge occurs without green required
CI for the exact current head, even after an acceptance or retry. A moved head
invalidates CI and review evidence. These rules do not grant merge authority.

Non-goals: a daemon, a REPL, a second queue store, implicit approval or an
unbounded autonomous repair. An advisory capacity warning is not permission to
violate writer custody or forge policy.

## Shared contracts

The following names and semantics are the interface between implementation
slices. They extend the general graph document and template compiler, not a
`deliver`-specific interpreter. Existing serialised graphs keep their sealed
plan and replay semantics; new fields are versioned and validated on admission.

| Interface | Contract |
| --- | --- |
| `loop` | A named bounded cycle with `max_rounds`, a persisted round and a typed outcome edge. A round is consumed once when its repair attempt is admitted, not on observation or replay. `max_ci_rounds` defaults to 3 CI repair attempts per rung. A red result at that bound advances to the next rung and resets the per-rung count; exhaustion of the final rung parks. A separately authorised edit may extend the bound. Admission budgets the expanded worst case. |
| `escalate` | Ordered route-tier aliases, default `[junior, senior, principal]`, on a node. The first attempt uses the first rung; a typed hygiene outcome or review repair that remains unsuccessful advances one rung; CI red advances at its per-rung bound, never past the final rung. The final rung's brief permits wider scope within actual authority; strict scope and custody still bind. Persist rung and cause. Exhaustion parks. Route unavailability remains distinct from an unsuccessful attempt. |
| `review` | `single` (default, one independent review after green CI) or `panel` (one existing review panel after green CI). `accept_inconclusive_without_blocking` defaults to `true`; only a valid, complete structured findings result with no blocking finding qualifies. Blocking severity applies even when a finding has no trigger. Missing or malformed evidence parks, never silently accepts. A FAIL or blocking finding routes to repair. |
| `plan` | Existing sourced boolean; default follows `domains.fabric.implement.plan`. When true, a read-only scope result joins the writer brief; failure or missing required plan parks instead of pretending a plan exists. |
| `hygiene` | A generic post-writer step runs the configured repository formatter in the retained worktree and commits only its changes. Outputs are `clean`, `no_commit`, `uncommitted_edits` or `error`, with before/after heads and handoff. Check all tracked and untracked edits before formatting; never absorb a writer's edits. Compare the attempt's input and output heads, not merely commits ahead of main. Formatter or commit failure is `error`, not success. The two named non-success outcomes feed escalation; errors park with diagnostics. |
| `park` | An idempotent ask/inbox request records graph/item/node/attempt, typed reason (`loop_exhausted`, `ladder_exhausted`, `no_commit`, `uncommitted_edits`, `ci_unavailable`, `conflict`, `review_evidence`, `operation_error`), bounded lane handoff, head and evidence references. One open request per parking transition; answer alone neither resumes nor grants landing authority. |
| Queue | `max_parallel` bounds active item graphs; `ci_slots` configures the `ci` admission class. Both have effective values and source. Zero CI slots uses the existing admission convention of disabling the cap. A cap wait is visible, bounded and warns before proceeding under the existing advisory admission policy; it cannot bypass an independent hard boundary. |

Launch preparation refusals (for example a retained worktree whose writer
lineage belongs to an earlier graph) are not unsuccessful attempts: reconcile,
then relaunch up to `domains.fabric.queue.prep_retries` (default 2) before
parking with `operation_error` and a diagnostic even if the host supplies no
message. An adopted PR or retained worktree must be adoptable by a new item graph without a lineage refusal. Every repair brief
carries the failing head, the bounded failure excerpt and an instruction to
sweep the whole defect class (other tests, help text and docs asserting the
changed behaviour), because single-test repairs dominate observed round counts.

Why: bounded retries, explicit evidence and durable decisions make replay safe
and let a project change policy without replacing the engine. The issue’s original
“escalates once, then parks” acceptance case applies to a two-rung ladder; the
later owner direction specifies three default rungs, so the default parks only
after the principal rung is exhausted.

## Graph and landing flow

For each item, the default `deliver` shape is optional scope → writer → hygiene
→ publish PR → conflict gate → CI gate → review → acceptance → land. A conflict
gate observes the PR before waiting for CI. `CONFLICTING` routes to a rebase
writer on the same retained worktree, then hygiene, publication and fresh
conflict/CI gates. Checkpoint validation uses the planned recovery path to keep
an initial conflict nonterminal while that path is pending. Unknown mergeability
is pending, not clean. If the head moves
while mergeability is read, the observation is pending on the new head and
identifies both heads; stale conflict evidence cannot direct a rebase. A failed CI gate
passes the failing head, job identities and bounded log excerpts as untrusted
inputs to `ci-repair`; after hygiene and publication, return through the
conflict gate to CI on the new head. Review failure similarly passes structured
findings to a repair writer, then re-enters the gates and obtains fresh review.
Both paths use the per-node ladder and bounded loops; no new commit is not proof
of a fix. A CI rerun, if offered, is an explicit sourced policy and records the
run and head; it cannot produce a green verdict from stale or absent checks.

Record each gate observation, loop transition, rung, launch intent and operation
receipt before its dependent effect. Replaying tick or recovering after a crash
must not duplicate a writer, PR, loop increment, inbox request or merge; reconcile
an uncertain external effect against its operation identity before retrying it.
Release or recover admission leases on completion, cancellation and crash, with
no leaked slot or concurrent duplicate ownership. At landing, re-read current PR
head, required CI and conflict state and use the forge's match-head precondition;
a changed or unknown head returns to the gates, not to merge. Existing landing
policy still decides whether the actor may merge.

Extend `graph/loops.rs` (`expand_loops`, `handled_gate`),
`graph/transitions.rs` (`apply_gate_result`, `admit_attempt`),
`graph/template.rs` (`TemplateLibrary`, `Compiler`) and typed records in
`crates/records/src/graph.rs`. Use `src/fabric/graph_execution.rs` and the
existing landing gate rather than a separate workflow executor.

## Queue and controls

`provenant run deliver --over issues:N,M,...` resolves tracker identities in the
selected repository; a tracker query is resolved to a persisted snapshot before
launch, so later query changes cannot silently add or remove work. Deduplicate
by project, tracker and item identity across active queues atomically, report
existing ownership, and allow an explicit new submission only after terminal
settlement. An explicitly adopted open PR binds its repository, issue, branch, worktree and
current head to the same item graph, starts at the conflict gate, and validates
existing custody before any mutation. Each item has its own graph, worktree and
outcome; one parked item
does not stop others. A parked item retains its graph identity without consuming
an active slot; before a control resumes work, it must reacquire capacity under
the same bounded, advisory admission policy. `max_parallel` is the active-item
concurrency target, not just a launch-rate limit; a bounded wait warns before
advisory overflow and records that overflow.
Admission of CI work uses `admit` class `ci` across queues in the same host admission domain, from publication through a conclusive CI observation; persist
the lease identity and reconcile it on restart. A zero CI slot cap is uncapped
for that request, but its lease remains counted against capped requests. Dynamic
slot locks are created under the CI domain lock and persisted before use. The
lease registry lock is a stable owner lock reused across acquisitions, including
uncapped requests which need no waiter directory. Waiting items retain their place
and show why they wait. A park transition persists the corresponding graph wake
with its state, so an interrupted transition can be replayed. `src/tick.rs` advances due graphs and retries pending
observations; `src/fabric/graph_status.rs` and the existing inbox show item,
phase, head, round, rung, active lane, last event, source, wait and parked handoff as the board. A stalled attempt and delivery progress share one selected-attempt last event and source in the digest. The checkpoint retains the bounded full writer handoff, including completed reports; the inbox request may show a truncated summary.

Controls act on the exact graph and recorded revision with operation-idempotent
receipts. `fix` is a chair intent mapped to `steer` for a live attempt or
`retry` with findings for a settled repair; `retry` uses the existing retry or
an admitted graph `edit` for an explicit bound change; `drop` uses `stop` and
closes the owned ask. `accept` records a reason via `answer` and an admitted
edit to the review disposition, then rechecks all landing gates. It cannot waive
CI, scope or merge authority. No new public verb is required: aliases, if
provided, are compositions of these operations, not a separate command queue.
Answer receipts and graph transition intents must reconcile on tick because an
answer alone does not resume a graph. Repeated or competing controls cannot
apply twice or target a newer attempt accidentally.

## Layering and migration

`TemplateLibrary` precedence is default, host, project, launch; a project or
agent acting through a launch layer may replace a node template, its brief, ladder, gate policy or
parameter, and invocation/item overrides win through the existing compiler.
Expose resolved values and sources in explain/status, seal the resolved plan
for replay, and validate overrides and their bounded expansion before admission.
A replacement must meet the same typed input/output and authority contracts. A
composite override changes its constituent nodes, not unsupported leaf fields on
the composite. No engine fork or custom JavaScript hook is needed.

`graph::template::seal_queue_knobs` resolves the three queue knobs over
`QUEUE_KNOB_PRECEDENCE` (default, host, project, launch), sealing the winning
`Sourced<usize>` value with its layer and optional origin; an undeclared layer
warns and is ignored. `seal_queue_knob` does the same for one knob. The host
supplies candidates keyed by `domains.fabric.queue.*`; absent keys keep their
shipped value and source. The `deliver` composite exposes `review_shape`
(`single` default, or `panel`) with the matching `review_template`
(`review` or `review-panel`), `ladder`, `max_ci_rounds`, `lenses`, and the
`failing_head`/`failure_excerpt` repair inputs. `widen_final_rung_brief` and
`repair_brief` produce the final-rung and repair brief text: the sweep-the-class
instruction is always carried by a repair brief, and untrusted heads and
excerpts are bounded before they enter it. An authenticated queue item binds its
compiled delivery template to typed hygiene, publication and conflict nodes,
then to one bounded recovery owner. A normal item provisions its managed branch
at one pinned base; an adopted item seeds its existing PR and retained worktree.
Both seal the registry's queue knobs, CI wait source and work-item identity into
their graph. Replays identify the item by its authenticated persisted request
and resume the sealed graph without re-resolving templates or a moving base.

The new default removes today's pre-CI `findings-panel` and post-CI
`review-panel` pair in `assets/templates/composites.toml`: a clean item costs
one writer and one single reviewer, plus optional scope and deterministic gates,
versus one writer and two panels today. Panel mode costs one panel, not two;
repair, scope and provider token costs vary, so no fixed monetary saving is
claimed. Existing in-flight graphs keep their sealed shape. After integrated
restart, concurrency, control and exact-head acceptance tests pass, retire the
interim `~/.local/state/chair-driver/fleet/fleet.mjs` and its operational state
only with its owner's deletion authority; migrate or finish active items first.
The reference log shows reattachment after restart, repeated red CI, principal
exhaustion and a no-commit flake rerun. It does not establish successful merges.
Unlike the reference's process-local semaphores and append-file inbox, the graph
store and admission receipts are authoritative across crashes.

## Implementation slices

Land the shared contracts above first, then parallelise the following scopes;
integration owns cross-slice wiring and any minimal conflict resolution. Tests
below are acceptance properties, not an instruction to run builds in this lane.

1. **Graph core and records:** `crates/records/src/graph.rs` and
   `crates/fabric-runtime/src/graph/{model,loops,transitions,candidate,refusal}.rs`
   plus their tests. Test bounded cycle admission, one increment per admitted
   repair across replay, red at the round limit advancing once to the next rung,
   restart in place and final-rung exhaustion, and rejection of an
   unbounded cycle. Own the shared serialised types before other slices wire them.
2. **Templates and policy:** `crates/fabric-runtime/src/graph/template.rs`,
   `assets/templates/` and template tests. Test both review shapes, sourced knob
   and layer overrides, optional scope propagation, widened final-rung brief,
   and unchanged replay of a sealed old plan.
3. **Execution and hygiene:** `src/fabric/graph_execution.rs`,
   `src/fabric/graph_inputs.rs` and their tests. Test clean formatter commit,
   no-commit, tracked and untracked edits, formatter failure, retained handoff,
   and conflict → rebase → new-head CI with no duplicate side effect on restart.
4. **Queue and admission:** `src/graph_interface.rs`, `src/fabric/interface.rs`,
   `src/tick.rs`, `src/admit.rs`, `src/commands/{tick,admit}.rs` and their tests.
   Test query snapshot and duplicate ownership, item concurrency, CI class
   contention and warning, recovery of a lease, and independent progress after
   a restart with an item parked.
5. **Inbox, status and landing:** `src/fabric/{graph_human,graph_status}.rs`,
   `crates/fabric-runtime/src/inbox.rs`, `src/pr/landing/` and their tests.
   Test idempotent park and controls, stale revision and conflicting answer,
   malformed review evidence, INCONCLUSIVE with and without blocking findings,
   and refusal to land on red, absent, conflicting or moved-head CI.

Why: the slices have separate file owners and share declared types and typed
outcomes, so parallel writers do not invent incompatible interfaces.


## Shared types

The wire additions require graph `schema_version: 2`; absent fields stay absent
when an old sealed plan or checkpoint is decoded and encoded. A queue item is
bound only after registry authentication of its persisted launch intent; static
admission checks the resulting typed graph before execution. Queue control
ledgers, review and adoption evidence, CI wait and quota facts are declared
host-owned record fields; malformed values fail decoding rather than being
silently treated as absent. Unknown extension fields still round-trip.

- `GraphNode.loop_body` is serialised as `loop`: its existing `max_rounds` is
  required and positive, and the owning node `id` is the loop name. Optional
  `LoopSpec.outcome_edge: LoopOutcomeEdge` has `outcome: LoopOutcome`
  (`succeeded` or `exhausted`) and target `node: String`. Optional
  `previous_bindings` maps a body node name used as `previous.<name>` to its
  explicit outer node for round one; later rounds resolve the same name to the
  preceding round's node. Each initial node must be a direct loop dependency,
  and published-head bindings must name a publication node. A general recovery
  loop may name a typed hygiene outcome or a CI, conflict or verdict body gate
  in `retry_on`; only a failed named node admits another bounded round, and its evidence
  stays in the checkpoint. Hygiene retries apply to `no_commit` and
  `uncommitted_edits`; an `error` parks. A retained hygiene output can carry the
  exact committed head into the next writer even when no new publication exists.
  The writer retains its checkout and any uncommitted edits; it must not replace
  them or accept a different head. After any successful publication, conflict,
  CI and review each check its exact head again. Its optional `selected_publication`
  names a body publication:
  success exports the receipt from the successful round, including its exact
  head. Optional `initial_publication` names the directly bound initial seed:
  if the entry gates all pass and every recovery round is skipped, the owner
  exports that seed's receipt. Landing admission requires conflict, CI and verdict
  entry gates for the seed as well as each recovery publication. Consumers must
  use the selected receipt and revalidate its head before landing.
  A queue item composes these graph mechanisms into one recovery owner. Each new
  repair head returns through hygiene, publication, conflict, CI and review;
  exhausted or incomplete evidence parks instead of selecting a stale head.
  `NodeProgress.delivery_loop: Option<DeliveryLoopProgress>` persists `name`
  and the number of admitted attempts in `round`.
- `GraphNode.escalate: Option<EscalateSpec>` contains `ladder: Vec<String>`
  (default `junior`, `senior`, `principal`) and `max_ci_rounds: usize` (default
  3). `NodeProgress.escalation: Option<EscalationProgress>` contains zero-based
  `rung` and `cause: EscalationCause` (`no_commit`, `uncommitted_edits`,
  `review_repair`, `ci_red`). The ladder and CI bound must be nonempty and positive.
- `GraphNode.delivery_review: Option<DeliveryReviewSpec>` contains
  `shape: ReviewShape` (`single` by default, or `panel`) and
  `accept_inconclusive_without_blocking: bool` (default `true`). The distinct
  name avoids the existing graph review request and template review protocol.
- `HygieneOutcome` contains `kind: HygieneOutcomeKind` (`clean`, `no_commit`,
  `uncommitted_edits`, `error`), `before_head`, `after_head`, and `handoff`
  (each optional). `ParkReason` has the eight snake-case values in the table
  above. These record types live in `provenant_records::graph`.
- `GraphDocument.queue: Option<QueueKnobs>` seals `max_parallel`, `ci_slots`,
  and `prep_retries` as `Sourced<usize>` values, with `value`, `layer`, and
  optional `origin`. Their keys are `domains.fabric.queue.max_parallel`,
  `domains.fabric.queue.ci_slots`, and `domains.fabric.queue.prep_retries`.
  Shipped defaults are respectively 4 (the existing batch default), 1 (the
  existing custom admission-class default), and 2. `max_parallel` is positive;
  zero `ci_slots` disables that cap. Later slices resolve overrides and seal
  their sources; this slice only defines the contract.
