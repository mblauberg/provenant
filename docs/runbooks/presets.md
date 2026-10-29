# Graph presets

Owner: Fabric maintainers
Anchor: [Fabric specification](../specs/fabric.md#pipeline-presets)

Use `scout-swarm` to map a problem and `implement-with-lens` to implement a
bounded slice with a review lens. Roles choose routing tiers, and the chair
chooses assignments, scopes, worktrees and acceptance evidence. A junior scout
swarm remains read-only; it can share read roots without Git writer contention.

Inspect the resolved defaults and explain the exact graph before a run:

```sh
provenant preset list
provenant preset show scout-swarm
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
provenant run --preset implement-with-lens --prompt-file slice.md \
  --worktree /absolute/path/to/writer \
  --set 'parameters.scope=["src"]' --wait 30m
```

The bundled presets expose these main parameters. Use `preset show NAME` for
all role briefs and defaults.

| Preset | Main parameters | Default work |
| --- | --- | --- |
| `scout-swarm` | `width = 4`, `read_roots = []` | Junior scouts, then senior synthesis |
| `implement-with-lens` | `worktree`, `scope = ["."]`, `subject.commit = "HEAD"`, `lens_tier = "senior"`, `delivery = "none"` | Senior writer, then senior refactor lens |
| `review-council` | `subject.commit = "HEAD"`, `worktree`, `correctness_tier`, `simplicity_tier`, `risk_tier` (each senior) | Advisory junior test lens, senior lenses and synthesis |
| `big-batch` | `lane_one_worktree`, `lane_two_worktree`, `batch_worktree`, `lane_one_scope`, `lane_two_scope`, `integration_branch = "batch/graph"`, `width = 4`, `read_roots = []` | Scout, parallel writers, council, principal refactor, two-round repair and principal review |

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
`route`; `route` accepts a pool name or a graph route table, and `effort`
configures it. A curated `high-junior` pool must be configured separately.
Increasing junior effort does not create that tier.
A read-only role's `worktree` becomes an additional read root; it acquires no
Git writer lock.

Each child preset keeps its own parameters and roles; an entry's `parameters`
or `roles` tables customise that child. Command-line overrides use TOML values:

```sh
provenant run --preset fan-out --prompt-file brief.md \
  --set 'parameters.width=3' --set 'parameters.route="junior"' --explain
```

Keep TOML string quotes inside shell quotes. Expansion refuses unknown fields,
cycles, more than eight levels and more than 512 nodes including children.
The original `single`, `chain`, `fan-out` and `fan-out-synthesise` structures
remain available; every slot, including synthesis, can contain a preset.

## Review and source subjects

The implementation lens examines redundancy, test quality, cleanup and rigid
over-engineering. `review-council` combines distinct test, correctness and
simplicity lenses with synthesis. Junior reviews are advisory when the author
is senior or principal. Counted acceptance reviews must be at or above the
author's tier under `HARNESS.md`. Add a principal lens for high-risk areas and
principal-authored refactoring. Synthesis reconciles the evidence and does not
turn advisory votes into acceptance.

Read roots permit scouting another worktree's uncommitted files. For a
combined review, bind `source` to an immutable commit, a direct dependency's
commit output or the graph's combined integration head:

```toml
source = { type = "commit", commit = "0123456789abcdef0123456789abcdef01234567" }
# Or, on a node directly following the writer:
source = { type = "node", node = "implementation" }
# Or, when integration_branch is configured:
source = { type = "integration" }
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

## Prepare a batch

`big-batch` groups a scout swarm, parallel senior implementation lanes, review
lenses, principal refactoring and repair. Prepare and register distinct writer
worktrees first. Partition their literal write scopes so writers can run
concurrently. Integrate lane heads into the explicit batch branch, review its
pinned combined commit, then use that subject for the serial principal and
repair contexts. Existing clean ancestor contexts fast-forward safely at
admission; divergent contexts need chair reconciliation. Automatic worktree
provisioning is outside this workflow.

Bind each batch context explicitly:

```sh
provenant run --preset big-batch --prompt-file batch.md \
  --set 'parameters.lane_one_worktree="/absolute/path/to/lane-one"' \
  --set 'parameters.lane_two_worktree="/absolute/path/to/lane-two"' \
  --set 'parameters.batch_worktree="/absolute/path/to/batch-writer"' \
  --set 'parameters.lane_one_scope=["src/one"]' \
  --set 'parameters.lane_two_scope=["src/two"]' \
  --set 'parameters.integration_branch="batch/graph"' --explain
```

Use read-only junior swarms for scouting and research. Give implementation
lanes to seniors or a curated high-junior tier, and put long-horizon principal
refactoring at batch end. Keep the refactor and over-engineering lens on most
implementation lanes. Choose review depth from the risk and author tier.

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
