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

## Model policy

Configure policy under `[domains.routing.model_policy]`. Run
`provenant route resolve MODEL` to inspect the effective policy and its sources,
and `provenant config check` to find invalid rules. The
[routing spec](../specs/routing.md) owns the routing behaviour.

## Review panels

Configure review shapes and selection under `[domains.routing.review]`; inspect
the schema for available settings. Preview a review before submitting it:

```sh
provenant run review --brief-file brief.md --set subject=origin/branch --set review_base=origin/main --explain
```

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
