# Routing and templates
Owner: provenant-routing
Anchor: [resolve.rs](../../tools/front-door/crates/routing/src/resolve.rs); [ADR 0039](../adr/0039-graph-of-templates.md#interface)

How to choose and steer routes, pace accounts and shape review panels. The
bundled catalogue (`tools/front-door/crates/routing/assets/catalogue.toml`) owns
default pool membership, weights and effort controls; the
[routing spec](../specs/routing.md) owns the rules. Everything below is
overridable in the selected state's `<state>/config.toml`.

## Inspect and choose

Run `provenant route list` to inspect pools, then `provenant route resolve NAME`
to preview a choice and its sources. Use `provenant route --help` to change
pools or exclude a route. Dispatch readiness can change after the preview, so
check the attempt record for the resolved route.

## Pacing

Configure account pacing under `[domains.routing.pacing]`; inspect the available
settings with `provenant config schema` and the result with
`provenant route resolve NAME`. Remove account references when unregistering an
account. For a temporary change, use `provenant route override --help`; inspect
and clear the override when it is no longer needed.

Monthly allowance accounts default to a 10 percentage point burst above elapsed
cycle time. The local launch ledger keeps estimates current while provider
billing lags; a missing or stale cycle is constrained. Inspect estimates and
sources with `provenant route list`.

Override the global defaults, or a single account, in the selected config:

```toml
[domains.routing.pacing.monthly]
burst_percent = 10
initial_launch_cost_basis_points = 100

[domains.routing.pacing.accounts."cursor/work".monthly]
burst_percent = 15
```

An account monthly table overrides the full global monthly profile; omitted
fields take shipped defaults. Set `enabled = false` in that table to bypass the
monthly cap, or set the account pacing `policy = "spend"` to stop pacing all its
windows. These preferences do not bypass exhausted allowance or authentication.

The account quota cache also persists the launch journal. Preparation reserves a
launch before returning its specification, under the quota publication lock.
The most recent 4096 attempt IDs deduplicate replayed preparation. A busy cache
skips the candidate for retry. Launches made with pacing bypassed are still
recorded, and failed launches retain their reservation conservatively.

Between reports, estimated spend is the last reported percentage plus launches
since the anchor times estimated cost. Positive provider-reported usage changes
calibrate cost by dividing the change by intervening launches, rounded up.
Subsequent samples blend 75% previous cost with 25% new cost. Before calibration,
the configured initial launch cost applies. Flat or decreasing readings in the
same cycle retain pending launch estimates. A new cycle resets spend but retains
learned cost. Timestamps keep launches at or after a slow probe's observation
pending even when they were recorded before it completed; insufficient retained
journal coverage never clears debt.

Monthly windows are recognised by billing-cycle/monthly/thirty-day pool names
or 28–31 day durations. An adapter's monthly-allowance declaration, account
monthly profile or remembered monthly ledger also requires evidence when a
snapshot omits its cycle. Missing, stale, future-dated, reset or incomplete
cycles are constrained. Other windows retain their existing treatment, and
monthly windows respect model-pool scope. Profiles can select other windows
with `windows`. The estimate averages launches across one account's models and
pools; it cannot measure individual launch costs or external usage before the
provider reports it.

## Model policy

Configure policy under `[domains.routing.model_policy]`. Run
`provenant route resolve MODEL` to inspect the effective policy and its sources,
and `provenant config check` to find invalid rules. The
[routing spec](../specs/routing.md) owns the routing behaviour.

## Review panels

Configure review shapes and selection under `[domains.routing.review]`; inspect
the schema for available settings. Preview a review before submitting it:

```sh
provenant run review-panel --brief-file brief.md --set subject=origin/branch --set review_base=origin/main --explain
```

The host pins both refs, then selects the panel from the changed paths and size
between their merge base and the subject. Configured path floors therefore apply
before the full panel expands, and the brief reaches its framing, lens,
verification and synthesis stages. A command such as
`provenant run review.security` runs one lens only; use `review-panel` for the
complete configured topology. For an explicit risk or risk tag, supply the
graph's top-level `review` request; `--set subject` and `--set review_base` pin
the range only. If path or size metadata is incomplete, Provenant applies the
configured unreadable-path floors and includes a warning in reviewer guidance.

Bind each attempt's resolved route to the review evidence. Review a changed head
again as required by the repository's workflow.

## Templates

Run `provenant templates` to discover templates and `provenant templates NAME`
to inspect one. Register a writer worktree or provide a branch and base, then
preview the submission with `provenant run --file delivery.toml --explain`.
Check its scope, authority and gates before submitting without `--explain`.
Use `provenant help --all` for parameters and graph operations. The
[template decision](../adr/0039-graph-of-templates.md) describes the model.

For command or host-native routes, configure named entries under
`[domains.fabric.commands]` or `[domains.fabric.host_native]`; use the config
schema and route help to select them. Command argv comes from host configuration,
not the task prompt.
