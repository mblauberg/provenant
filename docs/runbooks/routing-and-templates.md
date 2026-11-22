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
preview, so read the attempt record for the resolved route.

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
