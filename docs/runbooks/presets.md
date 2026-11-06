# Graph presets

Owner: Fabric maintainers
Anchor: [Fabric specification](../specs/fabric.md#pipeline-presets)

Use `scout-swarm` to map a problem and `generator-evaluator` to implement a
bounded slice with evaluating lenses. Roles choose routing tiers, and the chair
chooses assignments, scopes, worktrees and acceptance evidence. A junior scout
swarm remains read-only; it can share read roots without Git writer contention.

Eleven presets are bundled: `scope`, `single`, `chain`, `fan-out`,
`fan-out-synthesise`, `scout-swarm`, `generator-evaluator`, `review`,
`big-batch`, `scope-then-run` and `deliver`. All lower through the same stage path.
`preset list` shows a table of name, winning layer, level, cost and errors.
`preset show NAME` groups the resolved configuration into sections with origins, and
`preset explain NAME` shows when to use it, its cost and its typed parameters.

## Scope without repository writes

Use `provenant run --preset scope --prompt-file brief.md --explain` to inspect
a single senior scoping task. It reads the repository and returns affected
surfaces, a literal write scope, acceptance criteria, checks and open decisions.
`parameters.read_roots` adds repository read roots under the host's read policy.
The preset refuses expanded nodes with write access, write scope, delivery or
control actions, including overrides passed through a parent preset. Temporary
notes belong in attempt-private scratch.

The current runtime's `read_only` access also permits provider authentication
and state, tool, private-state and read-only exception writes. The preset
prevents repository write grants at expansion time but does **not** yet guarantee that
attempt-private scratch is the only writable location. Completing #2370 needs
a narrower runtime access mode and confinement grant policy, outside the preset
layer.

## Composition rules

A typed parameter can declare `bind = "risk_tier"` or `bind = "kind"` to receive
the corresponding `--risk-tier` or `--task-kind` request. An unbound flag warns
and is dropped. Parameter files and `--set` retain their usual precedence.

A parallel group can declare `zero_minimum = "all_but_one"` so a zero
`min_successful` requires all but one member, at least one.
It can declare `reduce_verdict = true` to export the worst successful member
verdict; missing or invalid verdicts count as inconclusive.
`export_first` adds outputs from the first member only; those outputs are
unavailable if that member fails.

`shape_stages` names the frame, parallel panel, verifier and synthesis stages
controlled by the selected review shape. When a subject is not yet pinned, one
deferred node builds those stages after the subject is known.

A `for_each` stage may declare `review_lenses = true` to apply the review
shape and risk-tag selection to its list.

A typed child parameter can declare `inherit = true` to receive a matching
parent parameter unless the stage supplies its own value. `inherit_explicit = true`
forwards only a parent override, leaving the child's own default otherwise.

A string parameter can declare `tracker_advice = true` to request tracker advice
for its supplied spec before a run.

A stage may declare `read_only_floor = true` to keep its preset read-only,
including through inheritance.

A leaf stage or role may declare `conditional_parameters`: each entry names a
source `parameter`, an `equals_any` list, a `value` and an `otherwise` value.
Interpolation uses the matching value when the destination parameter is absent;
an explicitly supplied parameter takes precedence.

Preset compilation checks composite nesting against the configured host depth
limit, then counts expanded composites and loop rounds against the node limit.
It uses the runtime's calculations; a refusal names the limit and its owner.

Typed parameters without defaults are required. Missing-value errors name the
parameter; `--set` and parameter files satisfy it and retain type validation.
Injected `read_roots` is optional for a nested preset that does not declare it;
other unknown parameters remain errors. `${index}` in nested role overrides
resolves for each child count copy. Node titles default to the concrete node ID;
stage titles and user or preset role overrides can supply a title.

Only a stage declaring `review_lenses = true` is governed by the review shape's
slot count and conditional risk tags. Other lists, including `big-batch` lanes,
expand in full.
Every omitted lens appears in graph provenance, `run --explain` and a warning.

## Select a preset by rules

When `run` has a prompt but no `--preset`, `--model` or `--route`, the bundled
rules select `single` by default. Readiness is checked against the prompt; a
prompt missing acceptance criteria, a risk tier, a verification route, a
settled design decision or a named surface selects `scope-then-run`. Explicit
`--preset NAME` remains the selection override. The bundled default remains
`single`; configure size-based choices in repository rules when their
parameters, such as writer worktrees, are available.

Repository rules live in `.provenant/preset-rules.toml`. They extend the
bundled defaults; maps merge by key and repository entries replace matching
keys. Rule values contain a preset and optional typed parameters:

```toml
[select]
default = { preset = "single" }

[select.by_risk]
critical = { preset = "review", parameters = { risk_tags = ["security"] } }

[select.by_kind]
docs = { preset = "chain" }

[select.by_paths]
"src/security/**" = { preset = "review" }

[select.by_size]
large = { preset = "big-batch" }

[select.by_readiness]
underspecified = { preset = "scope-then-run" }
```

Selection precedence is explicit preset override, readiness, most-specific
matching path, risk tier, task kind, size, then default. `*` and `?` match
within one path component; `**` can cross directories. The built-in size
bands are `small` (up to 16 KiB), `medium` (up to 64 KiB) and `large`.
Inspect a rule result and its source with `provenant preset explain`; supply
`--risk`, `--kind`, `--path`, `--size`, `--ready` or `--underspecified` to
explain a particular context. `run --explain` prints the selected preset and
source, and the choice is recorded in each graph attempt's work context.
On `run`, provide `--risk-tier`, `--task-kind` and repeatable `--path`
values for those selectors; prompt size is measured from the prompt input.

## Experiment with preset arms

Experiments are opt-in and live in project configuration under
`[domains.fabric.pipelines.experiment.NAME]`. They apply to a work item: run
with `--task-id` and automatic preset selection. The CLI rejects `--preset`
with `--task-id`, so an explicit preset never reads experiments. Experiments
are checked in name order, and the first one that accepts the work item
assigns it.

- `control` must be the preset the selection rules choose for this run. If it
  is not, the experiment is skipped.
- Each arm must be a preset that the same selection can use with this run's
  inputs. An arm that cannot run is skipped, and the run uses the control.
- `tiers` defaults to `routine`, `substantial` and `crucial`. A run without
  `--risk-tier` is never assigned, and terminal work is never assigned.
- `assign = "hash"` (the default) derives the assignment from the work-item ID.
  `assign = "epsilon"` assigns a candidate with probability `epsilon`, which
  defaults to `0.1`. Setting `epsilon` with `hash` is rejected.
- `expires` is an RFC 3339 instant after which the experiment stops assigning.
- `budget_usd` skips the experiment once its observed spend reaches the budget.
- `max_cost_ratio` sends a candidate back to control while its mean cost per
  row exceeds the ratio times the control's mean. Candidates wait until control
  rows exist.

Observed spend is the experiment's outcome rows from the last 14 days. It is
read at most once per run, and only when `budget_usd` or `max_cost_ratio` is
set. The report reads outcomes from the `main` branch, so a repository without
`main` cannot use a budget or ratio. If the report cannot be read, or a row's
cost cannot be read, the experiment is skipped and the run uses the ordinary
rules. Runs started concurrently read the same spend, so they can
overshoot a budget or ratio.

`shadow = true` assigns the candidate for comparison, but the run executes the
control graph, so the candidate cannot decide acceptance. The graph is the same
as a plain control run. Its context records `experiment_arm = "control"`, the
pattern `experiment:NAME:control`, and `shadow_of = "candidate"`. Shadow runs
therefore count as control outcomes.

An experiment with invalid settings prints `warning: experiment NAME skipped:`
with the reason on stderr, and the other experiments still apply. When an
experiment is valid but declines this work item, its reason is recorded under
`provenance.experiments` in the retained graph document.

```toml
[domains.fabric.pipelines.experiment.review-shape]
control = "single"
arms = { council = "review", chain = "chain" }
assign = "hash"
shadow = false
tiers = ["routine", "substantial"]
budget_usd = 25.0
max_cost_ratio = 1.5
expires = "2027-01-01T00:00:00Z"
```

Use `provenant telemetry experiments show review-shape` to compare mature,
measured outcomes within risk-tier and task-kind strata. It uses offline
pipeline outcome and ranking inputs; comparisons remain advisory. It cannot yet
pair shadow runs with their candidate, so shadow runs appear under control.

Inspect the resolved defaults and explain the exact graph before a run:

```sh
provenant preset list
provenant preset show scout-swarm
provenant preset explain scout-swarm
provenant run --preset scout-swarm --prompt-file brief.md --explain
provenant run --preset scout-swarm --prompt-file brief.md \
  --read-root /absolute/path/to/another-worktree --concurrency 4 --wait 30m
```

`--explain` creates no graph state. It reports the same admission result as
submission, separate inspection metrics, and pending source, worktree,
delivery-target, planner-authority and route-readiness checks derived from the
document. Human output then groups children under their composite and shows
roles, tiers, scopes, worktrees, selected inputs, attempt bounds, acceptance
and effective limits. `--json --explain` includes the exact graph and origins.
The same resolved graph has the same ID, so retrying its
submission observes the existing graph. Change the prompt or parameters when
you intend a separate run.

Run one implementation slice in an existing registered worktree:

```sh
provenant run --preset generator-evaluator --prompt-file slice.md \
  --worktree /absolute/path/to/writer \
  --set 'parameters.scope=["src"]' --wait 30m
```

The bundled presets expose these main parameters. Use `preset show NAME` for
all role briefs and defaults.

| Preset | Main parameters | Default work |
| --- | --- | --- |
| `scout-swarm` | `width = 4`, `external = 1`, `read_roots = []` | Junior codebase scouts and external scouts that research outward, then senior synthesis |
| `generator-evaluator` | `worktree`, `scope = ["."]`, `subject.commit = "HEAD"`, `base`, `spec`, `lenses`, `writer = ""` (role tier), `evaluator = "senior"`, `delivery = "none"` | Senior writer, the lens list over the commit it wrote, then an evaluator that gives each finding a disposition |
| `review` | `subject.commit = "HEAD"`, `worktree`, `base`, `spec`, `lenses`, `verifiers = 1`, `verifier_tier = "verifier"`, `risk_tier`, `review_shape`, `risk_tags` | One `lens.NAME` role per lens, a verifier, preferably from another model family, that tries to falsify the findings, then senior synthesis with a disposition per finding |
| `scope-then-run` | `width = 3`, `worktree`, `write_scope = ["."]`, `base`, `spec`, `delivery = "none"` | Scouts, a senior scoping stage, a verdict gate (a blocked scope launches no writer), then `generator-evaluator` |
| `big-batch` | `lanes`, `batch_worktree`, `base`, `spec`, `integration_branch = "batch/graph"`, `scout = 1`, `width = 4`, `read_roots = []` | Scout, one writer lane per `lanes` entry, council, principal refactor, two-round repair and review |
| `deliver` | `worktree`, `scope = ["."]`, `subject.commit = "HEAD"`, `delivery = "pr"`, `base`, `spec`, `checks = []`, `ci_timeout_seconds = 7200`, `max_iterations = 3`, `repair_tier = "senior"`, `decision_session`, `risk_tier`, `review_shape`, `risk_tags`, `land = 1`, `land_tier = "substantial"` | Senior writer opening a pull request, a CI gate on its head, up to `max_iterations` fresh repairs each followed by the gate, `review` on the newest head, then land; see [Deliver through CI to land](#deliver-through-ci-to-land) |

Write list parameters in a TOML file and pass `--params-file lanes.toml`. The
file holds top-level `name = value` pairs, one per preset parameter. `--set`
overrides it. For example, a three-lane batch:

```toml
batch_worktree = "/absolute/path/to/batch-writer"
base = "origin/main"
spec = "#2187"
lanes = [
  { name = "api", worktree = "/absolute/path/to/lane-api", scope = ["src/api"], spec = "#2187" },
  { name = "cli", worktree = "/absolute/path/to/lane-cli", scope = ["src/cli"], spec = "#2187" },
  { name = "docs", worktree = "/absolute/path/to/lane-docs", scope = ["docs"], spec = "#2187" },
]
```

## Configure roles and stages

Preset layers load from bundled defaults, user files under
`$XDG_CONFIG_HOME/provenant/presets/` (default `~/.config/provenant/presets/`),
then `.provenant/presets/` in the project. A repository file used during
resolution is announced before submission. `preset show NAME` displays the
effective values and their origins; invalid files appear in `preset list`
without blocking unrelated presets.

An existing name can override only its changed values. A new name can use
`extends` or define its own stages. For example,
`.provenant/presets/scout-and-check.toml`:

```toml
stages = [
  { id = "map", preset = "scout-swarm" },
  { id = "check", role = "checker" },
]
exports = [{ node = "check", outputs = ["result", "findings"] }]

[roles.checker]
tier = "senior"
title = "Check the proposed implementation boundaries"
prompt = "Challenge unsupported claims and missing call sites.\n${prompt}"
acceptance_checks = ["Every proposed change cites a relevant call site"]
```

An `exports` or `inputs` entry may select only outputs its node supplies:
every node supplies `result`, `findings`, `verdict` and `remaining`, while
`commit` needs write access or a pinned source (inherited from an enclosing composite), `branch` needs write access, and loops supply neither.
Expansion refuses a mismatch and names both nodes.

A node's prompt carries a bounded excerpt of each selected `result`. The full
upstream text is also written, redacted as
`provenant output --full` shows it, to a read-only file in the node's own
inputs directory, and the prompt lists the path beside the excerpt. A selected
composite stands for its exported members; a partial composite offers only
those that succeeded. The node may read that directory and nothing else of
another attempt's state. A node's files share a 2 MiB total; a result cut at
the limit ends with a marker, and the prompt names every capped result. An
offline or refusing upstream host is named unavailable with the reason, and
the node still runs. An older peer that does not report the redacted length
cannot supply a result that exceeds the remaining limit; it is also named
unavailable.

Stages run in order and receive the preceding result. An entry can select
other preceding outputs with `outputs = ["result", "findings", "commit"]`.
`parallel` contains heterogeneous entries, each with its own role or preset;
`count` repeats an entry from 1 to 32. Repeated roles can use `${index}` to
assign distinct work. `${prompt}` inserts the supplied brief. Exact parameter
placeholders retain their TOML type, so `write_scope = "${scope}"` can insert
a scope array rather than text.

Role profiles default to read-only access. A writer explicitly declares
`access = "worktree_write"`, an existing `worktree` and literal relative
`write_scope` paths. `tier` chooses a named routing pool, one route table or
ordered route tables. Use either `tier` or `routes`; `effort` configures a
single route, while lists carry effort per route. A read-only role's `worktree`
becomes an additional read root; it acquires no Git writer lock.

Two role fields shape review. `prompt_file` appends a bundled prompt, such as
`code-review/lenses/tests`, to the role prompt; it takes one name or a list and
refuses unknown names. `distinct_from = "writer"` routes the node away from
the model family that wrote its source; `distinct_from = "dependencies"` routes
it away from the families of every upstream attempt (the bundled verifier uses
it). Strength is `distinct_strength = "prefer"` (default) or `"require"`. Under
prefer, when no route outside those families is ready the router substitutes
one and the node records the separation as `unavailable`; under require the
attempt fails instead. Each node settles with a `separation` record (strength,
excluded families, applied family, `separated`, `unavailable` or
`unconstrained`), shown by `graph status`. With no known upstream family the
constraint has no effect.

An entry can fan out over a list parameter with `for_each = "${lenses}"`. Each
item needs a `name` of lowercase letters, digits and dashes. The node ID is
`ENTRY-NAME` and `${item.FIELD}` reads the item's fields in the role. Use
`for_each` or `count`, not both. An item with a `when` value runs only when
`risk_tags` contains that value.

Each child preset keeps its own parameters and roles; an entry's `parameters`
or `roles` tables customise that child. Command-line overrides use TOML values:

```sh
provenant run --preset fan-out --prompt-file brief.md \
  --set 'parameters.width=3' --set 'parameters.route="junior"' --explain
```

`fan-out-synthesise` accepts `parameters.branches` as a list of entries with
`id`, `brief` and `tier`. The list replaces `width`; each branch receives its
own brief and configured route. Set `parameters.synthesis_route` to route the
synthesis separately; it defaults to `route`. Without a branch list, `width`
and `route` retain their usual meanings. The planner replaces the branch stage
when `--plan on` is selected.

For `fan-out` and `fan-out-synthesise`, set `parameters.min_branches` to a
value from 1 through `width` to allow synthesis after some branches fail.
Omitting it requires every branch to succeed. A graph that meets its minimum
with failures reports `partial` and exits successfully; it does not deliver a
pull request. When the planner replaces the branch stage, its workers require
all-success instead.

Address a stage by its id instead of a path into its internals: on a stage
that runs a preset, `--set 'council.risk_tags=["security"]'` sets that
preset's parameter; on a role stage, `--set 'task.tier="junior"'` sets the
entry field. A stage without an `id` is `s1`, `s2` and so on by position.
`--set 'parameters.scope+=["docs"]'` appends to a list parameter instead of
replacing it (a single value appends one item). It is refused on a scalar
parameter, and on a role stage field.
Parameters can be typed tables (`type`, `default`, `min`, `max`, `enum`,
`optional`, `doc`); `preset explain` prints them. A `[pattern]` table (`level`, `summary`,
`citations`, `cost`, `select_when`, `tags`) says when a preset is worth using.

Keep TOML string quotes inside shell quotes. Expansion refuses unknown fields,
cycles, more than eight levels and more than 512 nodes including children.
`single`, `chain`, `fan-out` and `fan-out-synthesise` are ordinary stage
presets, so any stage of your own can run them.

## Role library

A stage names a role with `role = "NAME"`. Roles come from three layers, later
layers winning: the bundled `assets/roles.toml`, the user's
`$XDG_CONFIG_HOME/provenant/roles.toml` and the repository's
`.provenant/roles.toml`. A preset's own `[roles.NAME]` table wins over all of
them, and an inline role may `extends` a library role (even one of its own
name). A role is a profile: `extends` copies the parent and the child's keys
replace it. List the library with origins and reference digests using
`provenant preset show --roles`, or one role with `preset show --roles NAME`.

A dotted name is a variant, so an undefined `lens.house` uses `lens`.
`plan` takes a purpose through its variants `plan.scope`, `plan.slices`,
`plan.review` and `plan.research`; scouts are `scout.codebase` and
`scout.external`; lenses are `lens.NAME`; `review.verify`, `review.synthesis`,
`review.evaluator` and `write.implement` cover the rest.

| Role field | Effect |
| --- | --- |
| `card` | Short text placed first in the brief |
| `references` | `{ ref = "skill:SKILL/PATH", when = "HINT", inline = false }`. The compiler resolves the reference against the release `skills/` directory, records its content digest in the graph and lists it in the brief with its `when` hint. `inline = true` puts the text in the brief. An unknown reference, a `..` part or a link leaving `skills/` is refused. |
| `family` | `any`, `distinct_from_writer`, `cross_preferred` (`distinct_from = "dependencies"`, prefer) or `cross_required` (the same, require). A project makes the verifier strict with `[roles."review.verify"] family = "cross_required"` in `.provenant/roles.toml`. |
| `family_strength` | `prefer` or `require`; sets `distinct_strength` for `distinct_from_writer` or a cross-family role. |
| `fallback_tier` | Used, and recorded as a substitution, when `tier` names a route the routing configuration does not define. |

Provenance (origins, roles with digests and substituted routes) is kept in the graph document's `provenance` field. A role's `dispositions` list declares the values a synthesis must give each finding (`disposition` and `reason` in its findings block). Settlement enforces it: a findings block that is missing, or a finding whose `disposition` is outside the list or whose `reason` is empty, settles the node as failed and `advance_error` names the finding. Each node carries `role` and `role_digest` (a hash of the whole effective role and its reference digests), and `run --explain` lists every
reference with its digest, so editing a reference changes the digest. A node
that uses references may read the release `skills/` directory, read-only. A
role grants no authority: access, write scope and routing come from the stage.

`scout.external` looks outward: it asks for primary sources with URLs and
dates. It needs a web-capable route; define a routing route named `web`.
Without one the compiler falls back to `junior` and `run --explain` prints the
substitution. `--set parameters.external=0` opts out of external scouts, which
the origins record. The same applies to `verifier_tier` in `review`:
define a route named `verifier` in another model family. The role settings
`family = "cross_preferred"` and `family = "cross_required"` set
`distinct_from = "dependencies"`, excluding the families of all upstream
attempts. The low-level `distinct_from = "writer"` only separates a node
from its writer.

### Targeted local checks policy

Writer (`write.implement`), repairer (`repairer`), refactor (`refactor-principal`),
verifier (`review.verify`) and lens roles receive the targeted local check policy
via the `${local_check_policy}` placeholder. By default, roles run only the fast
targeted checks needed to prove their change locally:
- Run the tests added or modified and the named tests covering changed functions,
  with an explicit filter (`cargo nextest run -p CRATE -E 'test(NAME)'` or
  `cargo test -p CRATE NAME`).
- Use `scripts/select-affected-crate-tests` or `scripts/find-changed-tests` to help
  select targets.
- Run `fmt`, `clippy` for touched crates and generators for touched generated outputs.
- Never run a whole package, `--workspace` or the full suite locally; CI gates those
  on the pull request, and combined checks run at the batch head.
- Reviewers reproduce one finding with one targeted test.

A project can change this policy without editing role prompts by setting
`local_check_policy = "..."` at the top level of `.provenant/roles.toml`, or via
a non-empty `local_check_policy` preset parameter (for example,
`--set 'parameters.local_check_policy="..."'`). The effective source (`bundled`,
`roles.toml` layer or the parameter origin) and rendered role prompts are
visible in `provenant preset explain <preset>` under `local_check_policy` and
`prompts`. An empty parameter uses the layered roles-library policy: bundled,
then user, then project. Like `risk_tier` and `subject`, the policy is an ambient
parameter inherited by nested presets that declare it, including big-batch,
generator-evaluator, deliver, scope-then-run and review.

## Review and source subjects

The policy default is `review --plan on`. The configured path floors enforce
minimum review shapes, reviewer counts and required lenses for static, planned
and deferred reviews. A caller's list or planner fragment is expanded to meet
that count; omitted required lenses are added with a warning. See
the [review ladder](../../HARNESS.md#review-ladder).

`review` runs one `lens.NAME` role per entry of the `lenses` list: correctness
against the spec, design fit and reuse, simplicity, conventions and
readability, and tests. `security-privacy` and `concurrency-data-errors` join
when `risk_tags` names `security` or `concurrency`, and `adversarial` joins
when it names `terminal`; `risk_tier = "terminal"` counts as that tag.
`generator-evaluator` uses the same shape and lens defaults; an explicit
`lenses` parameter replaces its list. Every lens prompt
carries the same context. The reviewer has read access to the whole
repository, treats the diff stat and slices as navigation aids rather than the
scope, reads callers, existing utilities and architecture, and reviews against
the `spec` (an issue number or file) and the `base` (the diff reference). File
specs resolve first in the bound writer worktree, then in the launch checkout
root, then in the main checkout. Relative writer worktrees resolve from the
launch directory. Each
review brief includes the resolved file path, SHA-256 of the full file and an excerpt
(default 16,384 bytes); set `parameters.spec_max_bytes` from 1 to 65,536 to
change the cap. Planned big-batch reviews share one excerpt resolved from the
bound `batch_worktree`, then the project roots above. They do not resolve
specs from the worktrees the planner may assign to lanes. Missing or unreadable
files produce a named warning in explain and at launch, with guidance to pass
an issue-number spec instead. Issue numbers are passed through unchanged. The
frame states both when the shape includes it. Prompts live in
`skills/code-review/references/lenses/`. For a commit subject without an
applied plan, the quick shape skips framing and uses a deterministic reduction
of its lens verdicts; standard and deep run a verifier and synthesis by default.
Kind, path and risk-tag additions can add lenses to quick. Added refactor
lenses follow the configured catalogue order. The verifier tries to
falsify every finding, and synthesis gives each finding one disposition:
accepted, rejected or deferred, with a reason. The
`generator-evaluator` evaluator does the same over its lens findings. Its
`min_lenses` parameter defaults to 0 (the risk-tier review minimum, at least
one); set a positive number to require that many.
Preset explain shows the parameter and its origin. Unavailable lenses are
named in the evaluator input. A malformed findings block makes that lens
unavailable and counts against the threshold, just like a failed lens. The
evaluator receives bounded, severity-ranked findings and paths to the full
lens results; it must consult those results for findings omitted from the
bounded export.

A review of a `head` or `node` subject
(`deliver`, `generator-evaluator`, `big-batch`) cannot be measured when the
graph expands, because the commit does not exist yet. The graph (schema 2,
with `review_base`) carries one deferred `review` node with the panel stage
templates. When the subject is pinned the host measures it, picks the shape
and lenses, and builds the whole panel against that commit. Explicit `size` and
`review_shape` remain requests subject to the path floor. `--explain` shows
unmeasured paths and floors as pending.
`provenant review` uses content identity: repeating the same request attaches to
its graph, while `--fresh` starts a new graph. `--range` selects paths and size
from that Git range; the target remains the reviewed commit.

A `fan-out` stage with `distinct_families = true` (the `distinct_families`
parameter) labels its branches as one family group, so the scheduler prefers
different model families across them. It is a preference only: every branch
still runs.

`--fresh` makes a preset run a new graph even when its inputs are unchanged,
for example to rerun after a cancel. A plain rerun of the same inputs resolves
to the same graph, so it resumes or replays; `--fresh` adds a one-off nonce to
the graph identity. It requires `--preset`. After correcting a launch failure,
cancel the blocked graph and use `provenant run --preset <name> --fresh` to start
another.

Every `review` lens emits a fenced `findings` block before its single
verdict line. The block uses `schema_version: 1`, a `findings` array, and at
most 32 entries and one-line summaries; a lens with no
findings emits an empty array. A required
findings block missing from a lens result makes that lens unavailable.

The `review` lens group accepts partial success. A settled task failure
in a lens or verifier, or a verified local preparation refusal, may be retried
on its next route as a fresh attempt on the same node.
`domains.fabric.graph.limits.review_substitutions` bounds these
retries per member (default 1; 0 disables them). Graph status and a deferred
review's resolved panel record the substitute. An unresolved launch stays on
its original attempt. A replacement prefers a different family, while a fresh
attempt in the same family remains available when no other family is ready.

A lens that fails or
returns no valid findings block is recorded as unavailable with its cause, and
only successful lenses flow to verification and synthesis, after an
"Unavailable lenses" list that is never truncated. The group still fails with
`coverage_below_minimum: N of M lenses succeeded, minimum K; unavailable: ...`
when fewer than `min_reviews` lenses succeed. `min_reviews`
currently defaults by `risk_tier` (`substantial` 1,
`crucial` and `terminal` 2) and is at least 1. Model panels cap it at the
lens count; reduced panels retain the declared minimum, so a coverage shortfall
cannot produce approval.
A lens that ran and found blocking issues still blocks through its verdict.
Councils where every lens succeeds are unchanged.

Routing `review.floors` maps named path classes to a minimum risk tier and
required lenses. Project configuration can add classes or override the bundled
classes; configuration schema and explain show the source and applied floors.
Unmatched paths use the `unknown` class (substantial by default). Paths in the
routine documentation class retain their ordinary selection. Deferred subjects
show floors as pending until the commit is pinned.
A lens whose `tier` is empty takes its pool from the review selection for `risk_tier` or the
`review_shape` override. See [review shapes](model-routing.md#review-shapes)
for the shape rules and reviewer counts. Conditional lenses add to that count.
Set a lens `tier` to override the shape for that lens.
`--explain` prints the selected shape. Synthesis reconciles the evidence and
does not take a majority vote.

Read roots permit scouting another worktree's uncommitted files. For a
combined review, bind `source` to an immutable commit, a direct dependency's
commit output or the graph's combined integration head:

```toml
source = { type = "commit", commit = "0123456789abcdef0123456789abcdef01234567" }
# Or, on a node directly following the writer:
source = { type = "node", node = "implementation" }
# Or, when integration_branch is configured:
source = { type = "integration" }
# Or, the newest head of a writer and the repairs that continue it:
source = { type = "head", node = "write" }
```

These are alternatives, each declared once on the relevant role or entry.
Preset commit parameters accept refs such as `HEAD`; the front door resolves
them to full immutable object IDs before explain or submission. For example,
`--set 'parameters.subject.commit="refs/heads/batch/graph"'` pins a council's
subject. Raw graph commit bindings require the full object ID.
When a preset's default subject is `HEAD` and a writer or review worktree is
bound, the front door resolves that default against the worktree. An explicit
`subject` continues to take precedence, and a preset without a bound worktree
resolves `HEAD` from the invoking checkout. `--explain` warns when a node's
resolved commit source differs from the bound worktree's HEAD. A preset review
base expressed as a Git ref is resolved in the bound worktree before submission;
the resulting object ID stays in the graph when the ref later moves.
The runtime pins the source before launch and retains it on replay. A review
uses that exact subject. A later writer starts in an existing registered
worktree at that head, or a clean ancestor context fast-forwards to it while
holding writer exclusion. Dirty, diverged, reserved and live contexts are
refused. Ignored output is retained, including refusal if the source would
overwrite it. The runtime never resets a worktree. A repair's follow-up review
selects the repair's commit rather than reusing the earlier review head.

## Deliver through CI to land

`deliver` takes an approved slice from implementation to a merged pull
request without a chair watching CI:

```sh
provenant run --preset deliver --prompt-file slice.md \
  --worktree /absolute/path/to/writer \
  --set 'parameters.scope=["src"]' --set 'parameters.spec="#2222"' \
  --set 'parameters.decision_session="SESSION"' --wait 2h
```

The graph is `write`, then `ci` (a CI gate and its repair loop), `review`
(`review`), `accept` (a verdict gate) and `land`:

1. The `write.implement` writer commits in the registered worktree and the
   host pushes its branch and opens the pull request.
2. The CI gate observes the checks on the pushed head. While they are pending
   the node shows as running with `CI waiting elapsed=Ns checks=NAMES head=SHA`
   in `provenant fabric graph status ID` and the composite rows, and no model
   runs. A green gate skips the repair loop.
3. A red gate routes to the repair loop. Each round starts a fresh
   `repairer` agent on the same worktree and branch (`continues = "write"`).
   Its brief holds the failed checks with their log excerpts, the writer's
   final report and the previous repair's, never an earlier transcript. Each
   report that declared a handoff block also carries its `remaining` items,
   so work handed off reaches every round even when the report is clipped. The host pushes the repair to the same pull request
   and the CI gate runs again on the new head.
4. After `max_iterations` red rounds the loop stops. With
   `decision_session` set, the session receives a loop ask offering stop,
   extend or accept; without it the graph pauses, and
   `--extend loop.ci--repair.rounds=N` or
   `--extend loop.ci--repair.decision=stop|accept` resumes it.
5. `review` reviews a `head` source on the writer, the newest head the
   chain delivered, so a repaired head is the one reviewed. Its lenses that
   route away from the writer also route away from every repair author. A
   failed review stops the graph before land.
6. `land` merges that exact head under the host's land policy for
   `land_tier`. By default `routine` and `substantial` land automatically, and
   `crucial` and `terminal` ask the decision session. The ask names the
   command to run, `provenant land N --head SHA`; the land node waits and
   succeeds once it sees the pull request merged at that head. An inbox
   answer never approves the land: `hold`, or any other answer, fails the
   node. The session is bound to the land node itself, so it still applies
   when `deliver` runs inside another preset. Without a `decision_session`, a
   tier that asks fails the land node, which names the branch and head to land
   by hand with `provenant land`; it does not pause. The land node refuses a
   head other than the one the passing verdict reviewed, including a council
   pinned to an earlier head, and needs the CI gate green.

A repository `deliver.toml` or `roles.toml` cannot lower `land_tier` or turn
on `land` beyond your user configuration: each land node takes the stricter
tier, and a land your configuration turned off takes `terminal`. Set a lower
tier explicitly with `--set parameters.land_tier=...` when you mean it.

Knobs: `checks` names the required checks (empty uses the branch's required
checks), `ci_timeout_seconds` bounds each gate, `repair_tier` routes the
repairers, `risk_tier`, `review_shape` and `risk_tags` shape the council, and
`land = 0` stops after review and leaves the pull request open. Override the
repairer brief with a repository `[roles.repairer]` table in a same-name
`deliver.toml`. Pull requests open ready for review; draft pull requests are
not yet supported.

### Host-native repairs

After configuring the optional [host-native row](providers.md#host-native),
set its ordered repair routes in `.provenant/presets/deliver.toml`:

```toml
[parameters]
repair_tier = [
  { adapter = "host-native", name = "ci-fixer" },
  { alias = "senior" },
]
```

The repairer keeps its inputs, write scope and continuation of the writer's
branch. An absent host feature falls back to `senior` before launch. Each
completed repair is followed by the existing CI gate on the pushed head;
neither host success nor a fixer status check authorises review or land. The
default `repair_tier` remains `"senior"`. A tier selector accepts a pool name,
one route table or ordered route tables. Lists carry effort on individual
model routes or in the host entry; combining a list with role-level `effort`
is refused.

## Prepare a batch

`big-batch` groups a scout swarm, parallel implementation lanes, review
lenses, principal refactoring and repair. Prepare and register distinct writer
worktrees first. Partition their literal write scopes so writers can run
concurrently. Integrate lane heads into the explicit batch branch, review its
pinned combined commit, then use that subject for the serial principal and
repair contexts. Existing clean ancestor contexts fast-forward safely at
admission; divergent contexts need chair reconciliation. Automatic worktree
provisioning is outside this workflow. The scout stage is advisory: failed or
blocked scouting remains visible in graph status and does not stop the lanes.
Set `parameters.scout=0` to skip scouting when the lanes already have specs;
the default is `1`.

Create the batch worktree on its own branch, never on the integration branch.
Lanes, the principal and repairs all deliver into the integration branch, which
Git refuses to move while a worktree has it checked out, and graph admission
refuses such a graph. The graph fast-forwards the batch worktree to the
council's pinned integration head when the principal launches, and each
repair then continues from the head the previous delivery left there:

The graph creates a missing local integration branch at the graph's
`review_base` (the batch `base`, or `HEAD` when none), so only the batch
worktree needs preparing:

```sh
git worktree add -b batch/work /absolute/path/to/batch-writer BASE
```

Bind each batch context explicitly, one `lanes` entry per writer:

```sh
provenant run --preset big-batch --prompt-file batch.md \
  --params-file lanes.toml \
  --set 'parameters.integration_branch="batch/graph"' --explain
```

Use read-only junior swarms for scouting and research. For planned implementation
lanes, prefer junior for mechanical, well-specified work and senior for contained
design; use at most one principal lane per wave when appropriate. Override the
lane route as the work requires. Put long-horizon principal refactoring at batch end.
Choose review depth from the risk and author tier.

The checkpoint estimate reserves space for bounded node outputs and for each
deferred review. Disposition findings share a per-node budget: when inherited
copies exceed it, the checkpoint retains the bounded findings block and
reconstructs the copies from upstream inputs on replay. A warning records the
compaction; full results remain in their attempt files.

An unpinned review size resolves after its subject is committed. Admission
reserves its configured maximum panel, including the candidate templates and
inherited inputs. The runtime checks the prospective expansion against that
reservation before publishing it; an expansion outside the reserved shape is
refused with its projection and a remedy. This keeps the estimate conservative
without reducing the selected panel or changing the checkpoint limit. Use
`--explain` to check larger batches, and split a batch if the estimate exceeds
the limit.

A graph that reads `source = { type = "integration" }` pins the integration
head when that source first resolves, and the head joins the graph ID. The
graph stops before synthesis, and before a writer that starts from the pinned
commit, if the branch has since moved. Rerun the graph to pin the new head.
A fallback or retry writer that finds the pin already behind its own HEAD
continues; a HEAD that diverged from the pin is still refused.

## Read roots and planner limits

A node's `read_roots`, and a preset `--read-root`, must resolve inside the
project or its linked worktrees. Paths are canonicalised before the check, so
a symlink cannot point out of the project. The host allows other directories
through an absolute-path allow-list:

```toml
[domains.fabric.graph]
allowed_read_roots = ["/absolute/path/to/shared-reference"]
```

When a `run` brief, including a `--preset` brief, names an existing absolute or
`~/` path outside the effective read roots (bound worktrees, `read_roots` and
`--read-root`), submit and `--explain` print a warning and carry it in
`data.warnings`. Bare `/`, `~` and ancestors of the effective read roots are
skipped, and `[domains.run] brief_read_root_warnings = false` turns the warning
off. Copy the file into the worktree (for example `.agent-run/`) or
add its directory with `--read-root`; the warning does not change confinement.

A planner fragment cannot widen what its planner node holds. Each child
`role` and `source` must equal the planner's, and each child `read_roots` entry
must already be one of the planner's.

Planner fragment inputs may use the full graph shape, such as
`{"node":"writer","outputs":["result"]}`, or the shorthand `"writer"`,
which selects that node's `result` output. The planner prompt describes both
forms. This shorthand applies to fragments; static graph documents keep the
full `GraphInput` shape.

## Planner patterns

A plan node can also offer known-good patterns to its fragment. The plan node
carries the allow-list, so the runtime does not read preset files while
expanding a fragment. Each entry names the pattern, declares its typed
parameters and holds the pattern's nodes and exports:

```json
{"id":"plan","kind":"plan","access":"read_only","prompt":"Slice the work",
 "plan":{"kinds":["task"],"max_nodes":6,"patterns":[
   {"name":"generator",
    "parameters":{
      "worktree":{"type":"string"},
      "scope":{"type":"list","default":["."]}
    },
    "nodes":[
      {"id":"write","prompt":"Implement the change","access":"worktree_write",
       "worktree":"${worktree}","write_scope":"${scope}",
       "acceptance_checks":["The change is verified"]}
    ],
    "exports":[{"node":"write","outputs":["result","commit"]}]}
 ]}}
```

Parameter schemas use the preset forms `type` (`string`, `integer`, `number`,
`boolean`, `list` or `table`), `default`, `min`, `max`, `enum` and `doc`. A
parameter without a default is required. `min` and `max` are inclusive numeric
bounds for numbers, character counts for
strings, and element counts for lists; they do not constrain booleans or tables.
The fragment names a listed pattern
and supplies values; it cannot supply nodes, `access`, `role`, `source` or
`read_roots`. Those fields stay fixed in the pattern definition, and a
placeholder in one of them is refused, so a parameter cannot widen them.
Pattern nodes are prepared like ordinary fragment agents: they need a brief,
literal write scope and acceptance checks, and omitted routes resolve through
`senior`.

The fragment adds a `patterns` array next to `nodes`. An instance has a local
`id`, the pattern `name`, typed `parameters`, and optional `depends_on`,
`inputs` and `title`:

```json
{"schema_version":1,"nodes":[],"patterns":[
  {"id":"impl","pattern":"generator",
   "parameters":{"worktree":"/absolute/path/to/writer","scope":["src"]}}
]}
```

An instance expands to the same composite boundary, child IDs, exports and
scheduling as a statically written composite with the same template. The
pattern name and effective parameters, including defaults, are recorded with
the expansion. `plan.max_nodes` counts every expanded child against the host
node, edge, fan-out and depth ceilings.

An unlisted pattern, an unknown or invalid parameter, or a widening field
pauses the graph and admits no children. The pause reason names the pattern,
parameter or field. When the graph has a decision session, the pause also
raises the usual inbox decision.

Graph limits and host ceilings cover all children and every possible loop
round. The ordinary dependency-depth limit remains four. The default host
ceiling is 64, and `big-batch` explicitly requests 64. A host can choose a
lower ceiling; explain reports that effective cap before submission. The
[host runbook](fabric-hosts.md#configure-hosts) describes limits and budget
extensions.

An entry's `loop` can use ordered `stages`, including reusable roles and
presets, with an `until` gate and a positive `max_rounds`. For example, in a
preset that defines `repairer` and `reviewer` roles:

```toml
stages = [
  { id = "repair", loop = { max_rounds = 2, until = "accept", stages = [
    { id = "fix", role = "repairer" },
    { id = "review", role = "reviewer", source = { type = "node", node = "fix" } },
    { id = "accept", kind = "gate", condition = { type = "verdict", expected = "VERDICT: PASS", threshold = "all" }, inputs = [{ node = "review", outputs = ["verdict"] }] },
  ] } },
]
```

Existing raw `loop.body` configuration also remains available. All body
writers share one worktree; `previous.NODE` selects prior-round evidence or
commit bindings. Planner output cannot refer to named presets.


### Planner front-end for presets

`provenant run --preset big-batch --plan on` replaces the static `lanes` stage
with a principal planner that chooses from the preset's `[plan]` catalogue:
`writer-lane` (write scope, acceptance subset, route, budget) and
`wave-barrier`. Each planned lane runs the writer, a nested review and the
evaluator, as the static lane does. The planner sits after `scout` so the
plan uses the research; position is per preset. The planner may assign only the
registered writer worktrees: the project's linked worktrees by default, or
`--set 'parameters.worktrees=["/path/a","/path/b"]'`. Overlapping write scopes
inside one wave, a worktree outside the list and a budget above the graph
timeout refuse the plan before anything is admitted. A node's `deadline_seconds`
sets its lane timeout, capped by `limits.graph_timeout_seconds`.

`--plan` defaults to `[domains.fabric.plan] mode` (`auto`). `auto` stays off when
`lanes` (or the catalogue's other `explicit` parameter, such as `width` or
`lenses`) comes from a later preset layer or a dispatch override. An initial preset
layer supplies defaults regardless of its name. A `by_size` catalogue plans only a large subject. Otherwise, for
`big-batch` unset lanes force it on, and an unset `width` does the same for the
research presets; the thresholds
(`auto.acceptance_items`, `auto.batch_issues`, `auto.write_crates`) then only
change the reason that `--explain` prints. `--plan on` with explicit lanes
discards them and warns. `--plan on` on a preset with no catalogue is an error
on the command line; a configured `on` runs the static graph and says why.

The planner route is resolved the way dispatch resolves it, honouring
`--exclude`, `routing.exclude`, account evidence and quota, and trying each pool
member as dispatch does. If no member can serve
(excluded, exhausted, unauthenticated, undefined), or an extending preset
inherits `[plan]` without the stage it replaces, the preset runs its
deterministic defaults and the reason is recorded in the graph provenance and
printed as a warning.

If a planner node fails at run time, or succeeds with a fragment that cannot be
parsed, validated or admitted, the graph runs the preset's deterministic static
stage when one is configured and records `plan.fallback_reason` in the run
output. Without a fallback, an invalid fragment pauses the graph with its
reason. The static stage must settle successfully for the graph to succeed;
`provenant output` shows the fallback and its reason. Human
`provenant status <graph-id>`, `fabric graph status` and `fabric graph resume`
output include the bounded pause reason when a graph is paused.

A preset opts in with a `[plan]` table: `replaces` (the stage to swap),
`explicit` (parameters that, when set, keep the static graph), `guidance` and
one `[[plan.patterns]]` entry per pattern, written like a stage with
`parameters`, `nodes` and `exports`. Two optional keys scale the decision:
`by_size = true` makes `auto` plan only a large review subject, and
`subject_types` lists the `subject` types a planned fragment can pin (others
keep the static graph and say why). An entry with `for_each = "lenses"` stands
for one pattern per lens the review panel selects; `${item.name}` and
`${item.tier}` are bound in it.

Catalogues shipped today:

| Preset | Patterns | The planner chooses |
| --- | --- | --- |
| `big-batch` | `writer-lane`, `wave-barrier` | Lanes, scopes, waves, routes, budgets. |
| `review` | `lens-<name>` per panel-selected lens | Which of the panel's lenses run, and each tier. |
| `deliver` | The `review` lens catalogue | Its review panel and tiers at review launch. |
| `scout-swarm` | `scout`, `external-scout` | Scout count, each question, each route. |
| `fan-out`, `fan-out-synthesise` | `worker` | Worker count, each question, each route. |
| `scope-then-run` | `scout`, `external-scout`, `no-scout` | Whether to scout, the width, each question. |

`review --plan on` cannot add a lens: the panel (risk-gated lenses,
slot limit, order and tiers) builds the catalogue first, so the planner only
narrows it and raises or lowers tiers. A stage after a planned stage reads the
instantiated scouts', workers' or lenses' outputs, added to its inputs beside
the planner's result. `scope-then-run` runs a synthesis of its planned scouts
before scoping; `scout-swarm` keeps its own synthesis. `external = 0` removes
the external scout from the catalogue (`enabled_by = "external"`), and a
preset's `after` lists stages inserted behind the planner. Planned scouts keep
the caller's `read_roots`. `deliver --plan on` plans its review when the writer's
head is pinned, using the measured panel's catalogue and path floors. The
planner's rationale and chosen lenses are recorded in the graph; if it fails,
the deterministic panel runs. The CI gate and landing stages are unchanged.
`--plan off` retains the deferred deterministic panel. The chair plans when the brief
already fixes the shape (pass the parameter); for other catalogues leave
`--plan auto` when the brief leaves it open. For `deliver`, `auto` leaves the
review deterministic; use `--plan on` to request launch-time planning.

## Observe and resume

Preset runs use the normal bounded `run` wait, 240 seconds by default.
`--wait` accepts seconds or durations such as `30m`, up to four hours.
After submission, `run --preset` prints the graph ID and a ready-to-run
`provenant fabric graph status GRAPH_ID` command in both human and JSON output.
The resume, status and cancel verbs also accept the graph ID, retained document
path or its SHA-256 hash; the retained path and hash resolve to the document's
declared graph ID.
`--concurrency` accepts 1 to 16 and remains bounded by the host ceiling.
Timeout leaves work running. Graph status retains one composite status and
the children's attempts, findings and selected exports. Follow the printed
continuation command to inspect progress or resume an exhausted graph with
explicit limit or loop extensions.

A successful complete graph returns success. Failed or cancelled graphs,
paused graphs and wait timeouts return a nonzero status. The result names the
retained document and the continuation command, `provenant fabric graph
resume --document PATH`. Waiting advances the immutable document, including
pending CI gates, rather than reading status snapshots alone.

Failures in a child fail its composite and normally block downstream work.
Set `advisory = true` on a stage to allow its dependants to proceed after it
fails or is blocked; graph status retains the failure, and missing selected
outputs are labelled unavailable. A failed advisory stage still makes the
overall graph unsuccessful and prevents graph-level PR delivery. Graph status shows failed child outcomes; use
`provenant fabric graph status ID --full` for attempt details. An
exhausted bounded loop pauses for a chair decision; a synthesis does not
override failed acceptance evidence. Confinement, writer exclusion, immutable
sources and acceptance checks remain deterministic while the models choose
how to carry out each assigned role.

Generator-evaluator lenses and the evaluator review the implement node's settled
commit. Their host-written Git context compares that pinned commit with the
preset's `base`. When `base` is empty, the review uses the graph's pinned
integration base if available, otherwise the source commit itself, preserving
launches in repositories without `origin/HEAD`. Set `base` to the intended
comparison commit for a nonempty range. An `equal_base_and_head` node warning
names both commits when that comparison has no commit range to review. The host
sets the review verdict to inconclusive, even if the agent reported a pass. When
no base was resolved, the host reports that separately; the source fallback does
not identify an empty review range.

The `review` preset applies the same empty-range verdict to its review roles,
including synthesis. Read-only review lanes can run from a detached worktree;
pin `parameters.subject.commit` to select the commit. After correcting a launch
failure, cancel the graph and use `provenant run --preset review --fresh` to
start a new one.

## Writer tiers

Writer tiers are advisory. Set `parameters.writer` for `generator-evaluator` to
choose a writer alias; its empty default uses the layered `write.implement` role
tier. Planned `big-batch` lanes default to `junior`; use `senior` for contained
design and reserve `principal` for at most one lane per wave when appropriate.
`run --explain` reports concrete writer nodes by requested tier alias and warns
if higher tiers outnumber lower tiers. Unknown aliases and exact models are
excluded from the count, as are lanes a planner has yet to create. JSON includes
the counts, inversion flag and warning. The mix never refuses a graph or changes
a route.
