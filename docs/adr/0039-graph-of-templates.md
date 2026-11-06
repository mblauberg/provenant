# ADR 0039: One graph of template nodes

**Status:** Accepted

Changed: 2026-11-06, #2867.

## Context

The owner wants a graph to be a customisable collection of nodes that any agent
can use or create, with great presets, planners that can reshape it, and enough
trace to recover from failure, fix causes and measure efficiency. A plain lane
is a single node. Backwards compatibility is not a constraint; silently losing
state an install holds is.

Two independent architecture reviews read the code and agreed on the direction.
Today `src/lib.rs#run` splits a run into two lifecycle paths over shared
execution, with nine next-attempt mechanisms (#2869). Agents cannot add nodes:
`src/fabric/preset_graph.rs#run_preset_graph` refuses inside a lane, children are
read-only and `crates/run/src/launch.rs#RunStore::validate_child` rejects writers.
Planner fragments cannot emit composites or loops
(`crates/fabric-runtime/src/graph/plans.rs#validate_spec`). Roles, presets and
patterns are three templating forms. An eight-lane `big-batch` with file specs
exceeded the 16 MiB checkpoint limit because prompts are embedded per node.

## Decision

Keep the graph kernel, its checkpoint-before-launch discipline and the compiled
`GraphNode` as an internal form. Replace what sits above them with four concepts.

- **Attempt:** one provider process under custody. Retries and continuations are
  new attempts with lineage.
- **Template:** TOML with parameters and either a leaf (brief, tier, access,
  outputs) or `nodes`. It replaces role, preset and pattern. A composite is a
  template with nodes; a dotted name is a flavour (`review.security`). Layers are
  bundled, user, repository and inline, and the same schema serves all of them.
- **Node:** `use` plus overrides (`brief`, `tier`, `after`, `inputs`, `over`,
  `min_ok`, `repeat`, work binding). Its executor is an agent, a deterministic
  action (gate, decide, land) or a composite. Plan and synthesise are briefs, not
  runtime kinds. `over` copies a node per named item with `${item}`, replacing
  `count`, `for_each` and `parallel`, so a swarm is one node over named areas.
- **Graph:** a revisioned collection of nodes, changed only by patches.

```toml
# .provenant/templates/map-and-plan.toml
nodes = [
  { id = "map", use = "explore", over = ["crates/run", "crates/fabric-runtime"] },
  { id = "slices", use = "plan", after = ["map"] },
]
```

A dynamic list comes from a patch or from a resolver named `@name` at launch:
`over = "@review-panel"` selects the resolved panel, with the host choosing lenses from
risk tier and tags.

**One mutation path.** `admit_patch(graph, parent, patch)` is called by submission,
an agent's final `patch` block and a live call from a lane. A patch holds nodes,
inline templates and optional `skip` or `replace` of unstarted nodes. It commits
atomically against an expected revision and an idempotency key, and rejects cycles
and conflicting edits, scans for secrets and journals the change. Started attempts
are immutable. Limits are overridable, including child depth (default 8).
"Planner" is only `use = "plan"`: any agent may emit any template, loop, worktree
or further planner, so replanning after a wave is ordinary.

**Children.** A lane runs `provenant run` or `fabric_run`; the call crosses
`crates/fabric-mcp/src/lane.rs#HostRelay` and the host writes the journal. Children
join the same graph, sharing budget, cancellation and trace, and may be writers
integrating into the parent's branch. Spawning is recorded separately from
completion dependencies, so a parent can yield its slot while it waits. A composite
completes when its dynamic producers have closed and its join condition holds.
Cancellation follows the subtree unless a child was explicitly detached.

A child's roots and capabilities need not be a subset of its parent's. A lane
may request what its agent could request by calling the CLI directly; spawning
does not introduce a narrower authority envelope. The host applies the ordinary
launch policy to every child: credential roots, state roots and write roots
that overlap the worktree remain refused at launch.

**A plain lane** is a one-node patch using `task`, with `AttemptSpec` (#2869)
carrying placement, account and write roots.

**Prompts are content-addressed.** A checkpoint stores digests into a store beside
the journal, collected with its graph, so graph size does not grow with brief size
times node count. Prompts are scanned for secrets before they persist.

## Node library

| Template | Default brief's job | Tier | Composes as |
|---|---|---|---|
| `task` | Do the brief | senior | Leaf |
| `plan` | Return a patch and its reasoning | principal | Leaf; may add explorers |
| `explore` | Map an area and cite evidence | junior | Over areas, senior synthesis |
| `research` | Answer from dated primary sources | `web`, else junior | Over questions, senior synthesis |
| `design` | Options, root constraint, smallest slice | principal | Optional critique |
| `implement` (`.plan`, `.issue`, `.fix`) | Meet acceptance, commit, hand off | senior | Over slices, into one branch |
| `review` (`.<lens>`) | Find defects at a pinned head | by shape | Over lenses, senior synthesis; a panel of one is one attempt |
| `verify`, `synthesise`, `repair` | Falsify claims; reconcile inputs; fix qualified findings | senior | Repair loops with delta review |
| `ci`, `decide`, `land` | Deterministic control | none | Action |
| `deliver`, `batch` | Implement through landing | mixed | Composite |

The general `plan` brief carries the working rules: slices of about three
acceptance items with disjoint scopes into one integration branch; shared contracts
first, waves only for real dependencies; a long writer only for coupled work, near
500k tokens, perhaps a senior calling juniors; junior for mechanical work, senior
for contained design, at most one principal a wave; explore when evidence is thin;
end a wave with another `plan` when results could change the shape; light review per
slice and full review of the integrated head, with lens breadth before family;
consult recorded cost per template. Compulsory scouting and refactoring stages go.

## Trace and recovery

Add append-only `graphs/<id>.events.jsonl`, written before each checkpoint and
retained after terminal collection. It records patches (digest, author attempt,
reason, revision), node state changes with cause codes, attempt start and settle
(requested tier, resolved route), waits (slot, quota, CI, ask), gates, joins,
cancellation, budget use and effects (branch, commit, PR). It holds identifiers,
digests and codes only, so secrets stay out.

The checkpoint stays the authority for recovery, and operation replay prevents
duplicate launches. Recovery reconciles uncertain launches, reuses finished outputs
and resumes only unfinished work. `AttemptFact` gains `tier`, `template`,
`parent_node` and slot wait, and keeps its token, cache and timing fields, and reports group by
tier and template without counting composite descendants twice.

## Interface

`run`, `status`, `output`, `steer`, `stop`, `retry`, `edit`, `templates`, `land`,
`quota`. Everything else sits behind `--all`. `output --trace` shows the timeline;
`edit` applies a patch. `status` has one row schema for graph, node and attempt.
`run [BRIEF] [--use T] [--tier T | --model ADAPTER/MODEL] [--effort LEVEL]
[--account NAME] [--host NAME] [--over] [--set] [--file] [--explain] [--wait]` starts
work, and inside a lane adds a child. `retry` resumes, continues or redoes a target,
optionally with a new brief or a raised budget. `status --stats --by tier|template`
reports efficiency. MCP calls the same service with the same JSON and takes
documents, not file paths.

`--operation-id ID` is available in `help --all` for replay. Attempt controls use
`--set attempt.host='"workshop"'`, `--set attempt.build_heavy=true`,
`--set attempt.confidential=true` and `--set attempt.provider_timeout=300`.
`--host` overrides the attempt host setting. Delivery uses `--use deliver`.

Deleted: `dispatch`, `review`, `lanes`, `resume`, `cancel`, `preset`, `fabric graph`,
`fabric_graph`, `PRESET_INCOMPATIBLE`, the policy successors and tail chase (#2869),
child stores, `pattern_plan.rs`, per-preset `[plan]`, `shape_stages`,
`review_lenses`, automatic preset rules and pipeline experiments; review uses a
panel resolved at launch.

## Consequences

Delivery is one integration branch and one release, after a drain. Pending graphs
finish or are cancelled first and live lanes drain. Terminal checkpoints stay
decodable or take the existing `retire_unreadable` path, and the release note reports
any that do. Records that cannot be read are preserved and reported, never dropped.
User presets are ported by hand. Old child stores stop being written. Their
records drop out of statistics: this cutover does not ingest child-store history
into telemetry. The stores remain on disk for inspection; the cutover never
deletes them. All hosts upgrade together, then real launches run on each.

Rejected: event sourcing with the checkpoint as a projection, because rewriting
recovery adds risk for no present need; a state converter, because a drain leaves
little to convert.

Risks: every lane rests on journal recovery around external effects, so a parity
corpus gates the cutover; waiting parents can starve under lane caps, so they yield
slots; spend can run away, so budgets warn and pause; agents without a live relay
can only add nodes in their final block. IR additivity and relay-add locking are
inferred, not prototyped.
