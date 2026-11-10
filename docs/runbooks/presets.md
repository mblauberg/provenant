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

Register the writer worktree first with `provenant worktree create delivery --base BASE`, or pass `--branch NAME --base REF` to `run` to create an owned one.
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
shape and authority before submitting. `--wait` bounds settlement observation;
expiry leaves work running. Each top-level writer launch gets a
separate launch allowance of at least `domains.fabric.graph.writer_launch_seconds`
(default 55 seconds), or the remaining caller budget when longer. A child relay
request uses the same rule for its writer bindings. Binding may
therefore block beyond a short `--wait=1s` or `--wait=8s`; after binding, an expired
settlement wait returns the current graph without waiting again. The binding
allowance is sealed before writer provisioning and shared by later registry
locking, custody checks and launch retries, including existing worktrees. Child
relay requests use the same configured allowance and reserve response delivery
time, so allowances longer than 55 seconds still return a receipt or named refusal. A registry held past that allowance returns
`writer_lock_busy` with retry advice, before creating a branch or worktree.
Writer nodes default to `auto_continue_no_work = true`. When a writer declares
unfinished work, whether it committed, left nothing or left uncommitted edits,
the graph starts a fresh attempt in the same worktree with those edits intact,
with the original brief and its handoff as context. It selects another eligible route from the pool; if no
alternative is available, it returns control to the chair. A handoff naming an
owner decision or a missing capability uses `status = "blocked"` and also
returns to the chair without a launch. Session continuation is used only when the model's continuation policy
explicitly selects resume. The graph retains the first attempt and records the
continuation in patch history before launching. Set
`auto_continue_no_work = false` on a writer node, or in a leaf template's
`[node]` table, to disable this continuation. Uncommitted edits without a declared stop,
missing Git evidence, cancellation or a graph pause do not trigger it.

Bundled pools offer `codex/gpt-6.1-terra` only at low or medium effort. This
applies to both writer and read-only pool requests; an exact route or a user
pool can still request higher effort. Route comparisons should use the outcome
measures in `provenant telemetry stats`, checking the sample size and whether
assignments came from a hash-drawn pool before changing weights.
The `route_outcomes` rows group terminal attempts by route, access, effort and
pool assignment. Success requires a successful settlement without a retained
partial-completion signal. Cost per success uses the whole group's estimated
API-equivalent spend, including failures; time per success uses the whole
group's recorded provider milliseconds. Missing costs or times remain unknown,
and a group with no successes has no cost or time per success. Preference
choices, fallback draws and exact routes are marked observational. The success
rate is an attempt outcome, not evidence that a delivered change passed review.

The core verbs are `run`, `status`, `output`, `steer`, `stop`, `retry`, `edit`,
`templates`, `land` and `quota`; `help --all` reveals the rest.

## The `light` lane preset

`light` is a bundled preset for read-only or scratch work. It is configuration
on one graph node, not a template or a launch path, so it composes with every
template and a basic lane stays one node. It sets three per-node attempt
settings:

| Setting | `light` | Default | Effect |
|---|---|---|---|
| `attempt.isolation` | `shared` | `private` | Pinned read-only lanes use an immutable source view; unpinned read-only lanes read the bound checkout live. |
| `attempt.supervision` | `minimal` | `standard` | A silent provider is marked after one hour, not `lanes.silence_seconds`. |
| `attempt.receipts` | `none` | `delivery` | Requested writer delivery still runs and records its outcome. |

Apply it, or set any one setting alone, in a run document or a template's
`[node]` table, or per run with `--set`:

```sh
provenant run --use review --file brief.md --set 'attempt.preset="light"'
provenant run --use task --file brief.md --set 'attempt.isolation="shared"' --set 'attempt.receipts="none"'
```

An explicit setting beats the preset. The preset skips only steps a read-only
lane does not need: a read-only lane already has no writer worktree, dependency
install or branch. Pinned read-only lanes always use a snapshot of their pinned commit, including
under `shared`. Unpinned read-only lanes under `shared` read the bound worktree
or current directory live. Source registration and identity checks still apply
to pinned readers. Writers keep one
writer per worktree, so `shared` isolation is refused on a writer node. Secret
redaction, confinement and object integrity are not settings and stay on in
every preset. The matching knobs are `lanes.isolation`, `lanes.supervision` and
`lanes.receipts` in the [configuration reference](../reference/config-keys.md).

## Hand off long writers before context grows

Cost is context length multiplied by turns, so a writer that keeps working at a
very large context pays for it again on every call. `lanes.context_handoff_tokens`
stops a writer once one provider call's context reaches the threshold. The
graph then continues it in a fresh attempt on the same worktree, from the
writer's latest progress handoff. Commits and uncommitted edits stay in place.
The setting is off (0) by default, because restart has not yet been shown to
keep completion. The suggested setting is 200000:

```toml
[knobs]
"lanes.context_handoff_tokens" = 200000
```

Set it in a profile or host configuration `[knobs]` table to apply it to every
writer. Only providers that report each call's context are stopped (Claude
today); other lanes keep today's behaviour. The restart is the graph's one
automatic continuation for that writer, on another eligible route, so pool
routes suit it best. Compare spend before and after with
`provenant telemetry stats --since DATE --until DATE`: its spend by context band
(`context_bands` in `--json`) shows how much was spent above 200k.

## Deliver through CI to land

The `deliver` composite binds implementation, CI, bounded repair, independent
review and landing to one integrated head. Check every external action against
the authority envelope; a template grants no publication or promotion authority.

## Required provider families

A review panel can require a minimum number of lenses from a provider family
while keeping its route pools. Put the constraint in the run document's
`[review.minimum_family_counts]` table; a named lens can also set
`required_family`:

```toml
use = "review"
brief = "Review the pinned change."
[with]
subject = "HEAD"
review_base = "main"

[review.minimum_family_counts]
openai = 2

[[review.lenses]]
name = "correctness-spec"
required_family = "openai"

[[review.lenses]]
name = "tests"

[[review.lenses]]
name = "design-fit-reuse"
```

Run `provenant run --file review.toml --explain` before submitting the same
file without `--explain`. Selection checks each lens's pool against the normal
eligibility rules, then assigns enough eligible legs to each required family.
The named lens counts towards the panel minimum. Configured review shapes can
set the same `minimum_family_counts` table; a request can raise a shape's
minimum but cannot lower it.

An impossible requirement refuses the launch with the family, required count
and candidate exclusion reasons. The explanation and retained template
resolvers include `review-family-resolution`, whose slots record
`satisfied_family` for the legs assigned to satisfy the constraint. These
families are adapter registry data used for selection; they do not certify
provider protocol evidence for review assurance. Reviews without a family
constraint keep their existing selection and expansion.

## Review results

Review templates enforce findings, verification, synthesis dispositions and a
final verdict in code, including when the brief is overridden. An invalid reply
reports a contract error; a valid FAIL is a completed review. Full details live
in the [graph output contract](../specs/fabric.md).

Lens and verifier groups accept one successful member (`min_ok = 1`). Missing
coverage remains partial through verification and synthesis, so a partial panel
can report findings but cannot approve the change. Shapes without synthesis
reduce the lens and verifier verdicts together. A composite template may set
`reduce_verdict = true` to reduce exported verdicts; node overrides take precedence.

## Host-native repairs

`deliver` and `batch` collect qualified findings before repair: id, raising
lens, severity, file and line, evidence, proposed fix and a `trigger` with
`sequence`, `reach` and `observed`. Reach is `default_path`, `operator_mistake`
or `attacker_or_unreachable`; absent triggers parse as notes. Verification and
synthesis check each finding against the code, recording `triage`: `decision`
(`confirmed` or `rejected`), `reason`, `worth_fixing`, `root_cause`,
`blocking_class` and `shared_flaw`. Rejected findings stay in the record.
Writers receive only eligible confirmed findings, grouped by `root_cause` into
fix items, with the invariant under repair. Repair eligibility has one owner,
`graph/repair.rs#blocking`: checked blocking classes or medium/should-fix
findings with a default-path trigger. ADR 0035 boundaries and data loss remain
eligible regardless of reach; `kind = "spec_defect"` goes to the chair instead.

A repair writer reports `repair_items` in its findings block, each with an `id`
and `status`: `fixed`, `removed-the-case`, `disputed` (with a reason), or
`design-cannot-hold` (with an alternative). Disputes go to synthesis for judgement
rather than automatically counting as unresolved. Regression checks apply where
behaviour is observable. Writers can also report typed handoff `spec_concerns`
with criterion, claim, evidence, proposed_change and status (`as_written`,
`diverged` or `stopped`); graph status exposes these and spec-defect findings to
the chair.

Checkpoint concern previews share the bounded handoff projection with remaining
work. When previews are clipped, status keeps the full count, reports how many
entries are shown and points to the complete result; the typed handoff retains
every concern.

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

A checked shared flaw, an evidenced `design_defect`, a recurring blocking
finding or a writer's `design-cannot-hold` report escalates early. A third
review round takes the same read-only principal `design` path,
returning the root constraint, two or three options, a comparison of the
structural fix with the small one with a recommendation, and acceptance
evidence. Its findings block includes `repair_design` with
boolean `within_scope` and `one_way_door` fields. The host sends an idempotent,
non-blocking owner notification to the graph decision session (or the graph ID
when no session was supplied). A host `decide` node permits the continuation
without waiting only when the principal explicitly reports approved scope and
no one-way door. Changed scope, a one-way door or a missing assessment waits for
the owner; choosing hold prevents further writing. The decision request includes
the principal report. A continuation planner uses the bound `repair-slice`
template within the original literal write authority; approval alone does not
expand those paths. Round count prompts design judgement; it does not end the
work. The principal may recommend further repair through the continuation.
The final CI, review, acceptance and exact-head landing
checks still apply to the delivered head.

## Composition rules

Top-level runs use the template's `identity` policy, inherited by dotted
flavours. `identity = "fresh"` (the default, including `task`) starts a new graph
for every invocation. `identity = "content"` (`review` and `deliver`) reuses equal
submissions, including the template library, resolved panel, working directory
and resolved Git HEAD. A new commit therefore starts a new review. An explicit
`operation_id` on a run document replays that operation; an explicit `graph_id`
continues to select that graph. Use a new operation ID for new work.
Reattaching a cancelled graph preserves its cancellation; `retry GRAPH` resumes
it. A fresh template invocation starts a separate graph, including after a
cancelled invocation. Neither action bypasses writer custody.

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

Follow the [orchestrate skill](../../skills/orchestrate/SKILL.md) planner brief: about
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
