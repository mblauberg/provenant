# Graph presets

Owner: Fabric maintainers
Anchor: [Fabric specification](../specs/fabric.md#pipeline-presets)

Use `scout-swarm` to map a problem and `generator-evaluator` to implement a
bounded slice with evaluating lenses. Roles choose routing tiers, and the chair
chooses assignments, scopes, worktrees and acceptance evidence. A junior scout
swarm remains read-only; it can share read roots without Git writer contention.

Ten presets are bundled: `single`, `chain`, `fan-out`,
`fan-out-synthesise`, `scout-swarm`, `generator-evaluator`, `review-council`,
`big-batch`, `scope-then-run` and `deliver`. All lower through the same stage path.
`preset list` shows each one's level, summary and cost, and
`preset explain NAME` shows when to use it, its cost and its typed parameters.

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
critical = { preset = "review-council", parameters = { risk_tags = ["security"] } }

[select.by_kind]
docs = { preset = "chain" }

[select.by_paths]
"src/security/**" = { preset = "review-council" }

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

Inspect the resolved defaults and explain the exact graph before a run:

```sh
provenant preset list
provenant preset show scout-swarm
provenant preset explain scout-swarm
provenant run --preset scout-swarm --prompt-file brief.md --explain
provenant run --preset scout-swarm --prompt-file brief.md \
  --read-root /absolute/path/to/another-worktree --concurrency 4 --wait 30m
```

`--explain` creates no graph state. Human output groups children under their
composite and shows roles, tiers, scopes, worktrees, selected inputs, attempt
bounds, acceptance and effective limits. `--json --explain` includes the exact
graph and origins. The same resolved graph has the same ID, so retrying its
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
| `generator-evaluator` | `worktree`, `scope = ["."]`, `subject.commit = "HEAD"`, `base`, `spec`, `lenses`, `evaluator = "senior"`, `delivery = "none"` | Senior writer, the lens list over the commit it wrote, then an evaluator that gives each finding a disposition |
| `review-council` | `subject.commit = "HEAD"`, `worktree`, `base`, `spec`, `lenses`, `verifiers = 1`, `verifier_tier = "verifier"`, `risk_tier`, `review_shape`, `risk_tags` | One `lens.NAME` role per lens, a verifier, preferably from another model family, that tries to falsify the findings, then senior synthesis with a disposition per finding |
| `scope-then-run` | `width = 3`, `worktree`, `write_scope = ["."]`, `base`, `spec`, `delivery = "none"` | Scouts, a senior scoping stage, a verdict gate (a blocked scope launches no writer), then `generator-evaluator` |
| `big-batch` | `lanes`, `batch_worktree`, `base`, `spec`, `integration_branch = "batch/graph"`, `width = 4`, `read_roots = []` | Scout, one writer lane per `lanes` entry, council, principal refactor, two-round repair and review |
| `deliver` | `worktree`, `scope = ["."]`, `subject.commit = "HEAD"`, `delivery = "pr"`, `base`, `spec`, `checks = []`, `ci_timeout_seconds = 7200`, `max_iterations = 3`, `repair_tier = "senior"`, `decision_session`, `risk_tier`, `review_shape`, `risk_tags`, `land = 1`, `land_tier = "substantial"` | Senior writer opening a pull request, a CI gate on its head, up to `max_iterations` fresh repairs each followed by the gate, `review-council` on the newest head, then land; see [Deliver through CI to land](#deliver-through-ci-to-land) |

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

Stages run in order and receive the preceding result. An entry can select
other preceding outputs with `outputs = ["result", "findings", "commit"]`.
`parallel` contains heterogeneous entries, each with its own role or preset;
`count` repeats an entry from 1 to 32. Repeated roles can use `${index}` to
assign distinct work. `${prompt}` inserts the supplied brief. Exact parameter
placeholders retain their TOML type, so `write_scope = "${scope}"` can insert
a scope array rather than text.

Role profiles default to read-only access. A writer explicitly declares
`access = "worktree_write"`, an existing `worktree` and literal relative
`write_scope` paths. `tier` chooses a named routing pool. Use either `tier` or
`routes`; `routes` accepts pool names or graph route tables, and `effort`
configures them. A read-only role's `worktree` becomes an additional read
root; it acquires no Git writer lock.

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

Address a stage by its id instead of a path into its internals: on a stage
that runs a preset, `--set 'council.risk_tags=["security"]'` sets that
preset's parameter; on a role stage, `--set 'task.tier="junior"'` sets the
entry field. A stage without an `id` is `s1`, `s2` and so on by position.
Parameters can be typed tables (`type`, `default`, `min`, `max`, `enum`,
`doc`); `preset explain` prints them. A `[pattern]` table (`level`, `summary`,
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
the origins record. The same applies to `verifier_tier` in `review-council`:
define a route named `verifier` in another model family. The graph's
`distinct_from` separates only a node from its writer, so cross-family review
is a routing property for now, shown by `explain` as `cross_preferred`.

## Review and source subjects

`review-council` runs one `lens.NAME` role per entry of the `lenses` list: correctness
against the spec, design fit and reuse, simplicity, conventions and
readability, and tests. `security-privacy` and `concurrency-data-errors` join
when `risk_tags` names `security` or `concurrency`, and `adversarial` joins
when it names `terminal`. `generator-evaluator` takes the same list, with
correctness against the spec and simplicity by default. Every lens prompt
carries the same context. The reviewer has read access to the whole
repository, treats the diff stat and slices as navigation aids rather than the
scope, reads callers, existing utilities and architecture, and reviews against
the `spec` (an issue number or file) and the `base` (the diff reference). The
subject node states both. Prompts live in
`skills/code-review/references/lenses/`. After the lenses a verifier tries to
falsify every finding (`verifiers = 0` skips it), and the synthesis gives each
finding one disposition: accepted, rejected or deferred, with a reason. The
`generator-evaluator` evaluator does the same over its lens findings.

The routing review shapes are canonical for who reviews. A lens whose `tier`
is empty takes its pool from the shape that `routing.review.select_shape`
picks for `risk_tier` (or the `review_shape` override): `quick` runs one junior
reviewer, `standard` two, and `deep` up to six with its named lenses. Conditional
lenses add to that count. Set a lens `tier` to override the shape for that lens.
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
(`review-council`), `accept` (a verdict gate) and `land`:

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
   report the 4 KiB excerpt cuts short also carries its closing `remaining`
   window, so work handed off at the end of a long report reaches every
   round. The host pushes the repair to the same pull request
   and the CI gate runs again on the new head.
4. After `max_iterations` red rounds the loop stops. With
   `decision_session` set, the session receives a loop ask offering stop,
   extend or accept; without it the graph pauses, and
   `--extend loop.ci--repair.rounds=N` or
   `--extend loop.ci--repair.decision=stop|accept` resumes it.
5. `review-council` reviews a `head` source on the writer, the newest head the
   chain delivered, so a repaired head is the one reviewed. Its lenses that
   route away from the writer also route away from every repair author. A
   failed review stops the graph before land.
6. `land` merges that exact head under the host's land policy for
   `land_tier`. By default `routine` and `substantial` land automatically, and
   `crucial` and `terminal` ask the decision session first. The session is
   bound to the land node itself, so it still applies when `deliver` runs
   inside another preset. Without a `decision_session`, a tier that asks fails
   the land node, which names the branch and head to land by hand with
   `provenant land`; it does not pause. The land node refuses a head other
   than the one the passing verdict reviewed, including a council pinned to an
   earlier head, and needs the CI gate green.

Knobs: `checks` names the required checks (empty uses the branch's required
checks), `ci_timeout_seconds` bounds each gate, `repair_tier` routes the
repairers, `risk_tier`, `review_shape` and `risk_tags` shape the council, and
`land = 0` stops after review and leaves the pull request open. Override the
repairer brief with a repository `[roles.repairer]` table in a same-name
`deliver.toml`. Pull requests open ready for review; draft pull requests are
not yet supported.

## Prepare a batch

`big-batch` groups a scout swarm, parallel senior implementation lanes, review
lenses, principal refactoring and repair. Prepare and register distinct writer
worktrees first. Partition their literal write scopes so writers can run
concurrently. Integrate lane heads into the explicit batch branch, review its
pinned combined commit, then use that subject for the serial principal and
repair contexts. Existing clean ancestor contexts fast-forward safely at
admission; divergent contexts need chair reconciliation. Automatic worktree
provisioning is outside this workflow.

Bind each batch context explicitly, one `lanes` entry per writer:

```sh
provenant run --preset big-batch --prompt-file batch.md \
  --params-file lanes.toml \
  --set 'parameters.integration_branch="batch/graph"' --explain
```

Use read-only junior swarms for scouting and research. Give implementation
lanes to seniors, and put long-horizon principal refactoring at batch end.
Choose review depth from the risk and author tier.

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

A planner fragment cannot widen what its planner node holds. Each child
`role` and `source` must equal the planner's, and each child `read_roots` entry
must already be one of the planner's.

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

## Observe and resume

Preset runs use the normal bounded `run` wait, 240 seconds by default.
`--wait` accepts seconds or durations such as `30m`, up to four hours.
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

Failures in a child fail its composite and block downstream work. An
exhausted bounded loop pauses for a chair decision; a synthesis does not
override failed acceptance evidence. Confinement, writer exclusion, immutable
sources and acceptance checks remain deterministic while the models choose
how to carry out each assigned role.
