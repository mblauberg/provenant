# Routing and templates
Owner: provenant-routing
Anchor: [resolve.rs](../../tools/front-door/crates/routing/src/resolve.rs); [ADR 0039](../adr/0039-graph-of-templates.md#interface)

How to choose and steer routes, pace accounts and shape review panels. The bundled
[catalogue](../../tools/front-door/crates/routing/assets/catalogue.toml) owns pool
membership, weights and effort controls, and the [routing spec](../specs/routing.md)
owns the rules. Everything below is overridable in the selected state's
`<state>/config.toml`.

## Inspect and choose

`provenant route list` shows model sources and pools, and `provenant route resolve
NAME` previews a choice with its sources. `provenant route --help` lists the
commands that change pools or exclude a route. Readiness can change after the
preview, so read the attempt record for the resolved route. `provenant run --file
delivery.toml --explain` adds a `node_routes` row per launchable node: adapter, model
and effort with the source of each (`node field`, `policy`, `pool` or `default`). A
pool lists each candidate's model, adapter and effective effort with per-value
sources, rather than a pick, because the weighted draw, account and credential
decision are made at launch; a node-level effort overrides each candidate's
pool effort. Model-less adapter routes show the provider as `adapter` and keep
the configured entry name in `route_name`. Check those rows before submitting
a graph that depends on pool routing.

## Pacing

Pacing settings live under `[domains.routing.pacing]`; `provenant config schema`
lists them and `provenant route list` (or `--json`) shows the effective settings,
estimates, budgets and sources. Remove an account's pacing entries when you
unregister it. For a temporary change use `provenant route override`, and clear it
when it is no longer needed.

Monthly-allowance accounts may spend up to the elapsed cycle fraction plus a burst
(10 percentage points by default). Override the global monthly profile, or one
account's:

```toml
[domains.routing.pacing.monthly]
burst_percent = 10
initial_launch_cost_basis_points = 100

[domains.routing.pacing.accounts."cursor/work".monthly]
burst_percent = 15
```

An account's monthly table replaces the whole global monthly profile, with omitted
fields taking shipped defaults. `enabled = false` in that table bypasses the monthly
cap, and an account `policy = "spend"` stops pacing all its windows; `windows`
selects which windows a profile covers. None of these bypass an exhausted allowance
or authentication. How the launch ledger estimates spend, books costs and handles
missing cycles is in the
[routing spec](../specs/routing.md#monthly-allowance-ledger).

## Model policy

Configure policy under `[domains.routing.model_policy]`. `provenant route resolve
MODEL` shows the effective policy and its sources, and `provenant config check`
finds invalid rules. The [routing spec](../specs/routing.md) owns the behaviour.

## Catalogue and price overlays

The bundled catalogue (`tools/front-door/crates/routing/assets/catalogue.toml`)
and the bundled price sheet (`config/telemetry-prices.toml`) can be extended
without a rebuild. Owner entries live in the same config files the other
cross-cutting domains use, resolved in this order:

1. bundled asset (always present);
2. host `<state>/config.toml`;
3. host-project `<state>/projects/<project-key>/config.toml` (reported as
   `host`);
4. project `<project>/.provenant/config.toml`.

Later layers replace an entry with the same key or add it; they never merge
one entry into another. A catalogue model is keyed by `model`, a pool member by
`(pool, model)`, a capability-preference row by route name, and a price by
`(provider, model)`. Removal is explicit with `remove = true`. Removing a model
also drops its pool memberships and writer-advice references.

```toml
# <state>/config.toml or <project>/.provenant/config.toml
[[catalogue.models]]
model = "vendor/new-model"
capability = "strong"
efforts = ["low", "high"]
effort_suffix = true
evidence_ref = "vendor model card 2026-11-01"
family = "new"
service_tiers = ["standard"]

[[catalogue.pool_members]]
pool = "senior"
model = "vendor/new-model"
weight = 3
efforts = ["high"]
effort = "high"

[catalogue.capability_preferences.senior]
strong = 5
frontier = 7

[[prices.models]]
provider = "vendor"
model = "vendor/new-model"
billing = "api"
sources = ["https://vendor.example/pricing"]
[prices.models.rates]
uncached_input = 1000000
cache_read = 100000
cache_write = 1000000
output_excluding_reasoning = 4000000
```

An entry is a full replacement: fields left out take type defaults rather than
the values in the layer below. Per-model policy rules remain owned by
`[domains.routing.model_policy]`; the catalogue overlay carries facts only.
Weights are integers 0–100, `efforts` must be a subset of the model's efforts,
and a zero rate needs `zero_rate_verified = true`. To remove an entry:

```toml
[[catalogue.models]]
model = "vendor/old-model"
remove = true

[[catalogue.pool_members]]
pool = "senior"
model = "vendor/old-model"
remove = true

[catalogue.capability_preferences.senior]
remove = true

[[prices.models]]
provider = "vendor"
model = "vendor/old-model"
remove = true
```

An unreadable or malformed layer, or an entry that is malformed or would leave
the effective catalogue or price sheet invalid, is skipped with a warning that
names the layer and the reason; the remaining entries still apply. `route list`
reports `models[].source`, pool members' `membership_source` and
`weight_source`, `catalogue_sources` and `overlay_warnings` in JSON, and the
same sources and warnings in human output. `route resolve` reports
`catalogue model source`, `catalogue pool membership source`,
`catalogue weight source` and `catalogue capability preferences ... source`.
`telemetry stats` reports `price_sources`, per-price `source=` lines and
skipped-entry warnings. Sources are `bundled`, `host` or `project`.

## Review panels

Configure review shapes and selection under `[domains.routing.review]`. Preview a
review before submitting it:

```sh
provenant run review-panel --brief-file brief.md --set subject=origin/branch --set review_base=origin/main --explain
```

The host pins both refs, then selects the panel from the changed paths and size
between their merge base and the subject, so configured path floors apply before
the full panel expands. If path or size metadata is incomplete, the configured
unreadable-path floors apply and reviewer guidance carries a warning. A command
such as `provenant run review.security` runs one lens only; use `review-panel` for
the configured topology. `--set subject` and `--set review_base` only pin the range;
for an explicit risk or risk tag, supply the graph's top-level `review` request.
Bind each attempt's resolved route to the review evidence, and review a changed
head again as the repository's workflow requires.

## Templates

`provenant templates` lists templates and `provenant templates show NAME` shows
one's nodes and parameters. Register a writer worktree or give a branch and base,
then preview the submission with `provenant run --file delivery.toml --explain`;
check its scope, authority and gates before submitting without `--explain`.
`provenant help --all` has the parameters and graph operations;
[ADR 0039](../adr/0039-graph-of-templates.md) has the model.

For command or host-native routes, define named entries under
`[domains.fabric.commands]` or `[domains.fabric.host_native]`. Command argv comes
from host configuration, never the task prompt.

### Batch template

`batch` implements several slices into one `integration_branch`, then reviews it
and repairs within a bound. `items` takes bare strings (default parameters) or
tables:

- `id`: the slice's unique identifier.
- `brief` or `brief_file`: the prompt, or a file holding it (not both).
- `route` or `model`: a per-slice override (not both).
- `write_scope`: advisory paths the slice may modify.
- `after`: slice IDs that must settle successfully first. An unknown ID or a
  cycle is rejected at compile time, naming the IDs.

`max_parallel` caps concurrent slices. It defaults to
`domains.fabric.batch.max_parallel` (shown with its source by `--explain`) and
`--set max_parallel=N` overrides it per run. Each slice gets its own managed
worktree under the worktree layout; the batch `worktree` parameter stays the
checkout for review repair. A slice with `after` starts from the integration
branch once its prerequisites have integrated; independent slices start from the
graph's pinned integration base. `provenant status` shows a queued slice as
`waiting: after` (prerequisites unsettled) or `waiting: cap` (dependencies met,
cap full).

Each slice integrates into `integration_branch` as it settles. A merge conflict
pauses only that slice (`blocked`, with the conflicting files reported) while
independent siblings continue.
