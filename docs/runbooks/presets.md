# Templates and composites

Owner: orchestrate skill
Anchor: [ADR 0039](../adr/0039-graph-of-templates.md#interface),
[grammar.rs#Command](../../tools/front-door/crates/cli/src/grammar.rs)

Start with a bundled template. Use `task` for one assignment, `plan` for open
shape, `implement` for an approved slice, `review` for a pinned subject,
`batch` for parallel implementation and `deliver` for implementation through
landing. `explore`, `research` and `design` gather evidence or settle options;
`verify`, `synthesise` and `repair` check claims, reconcile inputs and fix findings.

```sh
provenant run --route junior --file brief.md --explain
provenant run --use deliver --file brief.md --set 'worktree="/absolute/project/.worktrees/delivery"' --set 'write_scope=["src"]' --explain
provenant run --use deliver --file delivery.toml
provenant status
provenant output TARGET
```

Register the writer worktree first with `provenant worktree create delivery --base BASE`.
Use its absolute path and literal write scope. For `delivery.toml`, bind the
same parameters in a reusable node document:

```toml
use = "deliver"
[with]
worktree = "/absolute/project/.worktrees/delivery"
write_scope = ["src"]
brief = "Meet the approved acceptance criteria and run the named checks."
```

`templates` inspects the library. `run [BRIEF]` accepts `--use`, `--tier`,
`--over`, `--set`, `--file`, `--explain` and `--wait`. Inspect the resolved
shape and authority before submitting. A wait expiry leaves work running.
The core verbs are `run`, `status`, `output`, `steer`, `stop`, `retry`, `edit`,
`templates`, `land` and `quota`; `help --all` reveals the rest.

## Deliver through CI to land

The `deliver` composite binds implementation, CI, bounded repair, independent
review and landing to one integrated head. Check every external action against
the authority envelope; a template grants no publication or promotion authority.

## Host-native repairs

`deliver` and `batch` collect qualified findings before repair: id, raising
lens, severity, file and line, evidence and proposed fix. Synthesis retains
these fields and their qualified identities, including when it uses a model.

Set `max_repair_cycles` to one, two or three (default three). This counts repairs
after the first review and is separate from CI's `max_iterations`. Each repair
starts a fresh writer attempt. Prefer a route outside the reviewers' families;
when none is available, continue on the available route and retain the separation
warning. The raising lenses review the delta and relevant tests. Risk or scope
growth requires a full panel re-plan through `@review-panel`. The host also
checks repair diffs for newly touched paths and changes to risk-tagged paths
already reviewed; this is a loose escalation guardrail rather than a refusal.
Blocking findings from a replanned panel feed the next repair round within the
same cycle cap. Later repair commits re-resolve that broader panel, so newly
selected lenses also review closure.

An evidenced `design_defect` or a recurring blocking finding escalates early.
Exhausting the repair limit takes the same read-only principal `design` path,
returning the root constraint, two or three options, the smallest viable slice
and acceptance evidence. Its findings block includes `repair_design` with
boolean `within_scope` and `one_way_door` fields. The host sends an idempotent,
non-blocking owner notification to the graph decision session (or the graph ID
when no session was supplied). A host `decide` node permits the continuation
without waiting only when the principal explicitly reports approved scope and
no one-way door. Changed scope, a one-way door or a missing assessment waits for
the owner; choosing hold prevents further writing. The decision request includes
the principal report. A continuation planner uses the bound `repair-slice`
template within the original literal write authority; approval alone does not
expand those paths. Review repair loops cannot be extended or accepted as
is to bypass the cap. The final CI, review, acceptance and exact-head landing
checks still apply to the delivered head.

## Composition rules

Top-level runs use the template's `identity` policy, inherited by dotted
flavours. `identity = "fresh"` (the default, including `task`) starts a new graph
for every invocation. `identity = "content"` (`review` and `deliver`) reuses equal
submissions, including the template library, resolved panel, working directory
and resolved Git HEAD. A new commit therefore starts a new review. An explicit
`operation_id` on a run document replays that operation; an explicit `graph_id`
continues to select that graph. Use a new operation ID for new work.

A template is TOML with parameters and either a leaf brief, tier, access and
outputs, or `nodes`. A composite is a template with nodes; a dotted name such
as `review.security` is a flavour. Layers are bundled, user, repository and
inline. Put repository templates in `.provenant/templates/`.

```toml
nodes = [
  { id = "map", use = "explore", over = ["src", "tests"] },
  { id = "shape", use = "plan", after = ["map"] },
]
```

A node is `use` plus overrides: `brief`, `tier`, `after`, `inputs`, `over`,
`min_ok`, `repeat` and work binding. `over` copies a node per named item,
binding `${item}`; `@name` selects a list resolved at launch, such as a review
panel. Planning and synthesis are briefs. Gates, decisions and landing are
deterministic actions. Use bounded repair with independent delta review.

Any agent may add nodes, inline templates, composites or further planners in a
patch. `edit` applies patches with an expected revision and idempotency key;
skip or replace only unstarted nodes. Admission is atomic, checks cycles and
conflicting edits, scans for secrets and journals the change. Started attempts
are immutable. Planner fragments and edits may omit a writer's `worktree`; the
host provisions one before launch and recovers interrupted provisioning. Explicit
worktrees must already be registered. Child depth defaults to eight and limits
are overridable within authority. MCP uses the same service and JSON with documents rather than paths.

Inside a worker, `run --use TEMPLATE` adds children to the same graph. They share
budget, stopping and trace. Template access determines whether a child writes;
a read-only parent can launch a writer. Writers use registered worktrees and
may integrate into the parent's branch. Writers with overlapping scopes share
a worktree and run in sequence; disjoint scopes can use separate worktrees. `--over`, `--set` and inline documents also
work inside a worker. Repeating the same launch under the same logical parent
returns its existing children, including after a retry or continuation.
A waiting parent yields its slot. A composite completes when its dynamic
producers close and its join condition holds.

## Writer tiers

Use the [planner brief](../../skills/orchestrate/references/planner.md): about
three acceptance items per slice, disjoint scopes, one integration branch,
shared contracts first and waves only for dependencies. Default slices to
junior; raise tiers for contained or difficult design. Add light review per
slice and full review of the integrated head. Scouting and refactoring need a
concrete purpose.

## Observe and recover

`status` uses one row schema for graphs, nodes and attempts. `output TARGET`
collects evidence; `output TARGET --trace` shows the timeline. Advanced `trace` inspection is listed in `help --all`. Events retain microsecond UTC timestamps, readable identifiers, digests and cause codes for
patches, attempts, waits, gates, joins, budget and effects. Checkpoints own
recovery; replay reconciles uncertain launches and reuses finished outputs.

Use `steer` for provider-supported messages, `stop` for a subtree and `retry`
to continue or redo a target, optionally with a new brief or authorised budget.
Use `edit` to reshape unstarted work. A synthesis never waives a failed gate.
`status GRAPH` prioritises current failures and their cause codes, with a next
command inside a 2 KiB reading. `status --stats --by node`, `--by tier` and
`--by template` report tokens, cache read share of input, wall clock, slot queue
time and outcome counts from attempt facts. Missing cache and queue measurements
are counted as unknown. Attempts are counted once, without composite totals. Land only after exact-head CI
and required independent review, within the repository's authority.
