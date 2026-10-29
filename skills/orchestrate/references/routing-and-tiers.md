# Routing and tiers

The Rust routing/providers/quota owners define route selection. Inspect the
current catalogue with `provenant route list`; use `--json` for records.

## Native first

Use native subagents for bounded work within the current provider. Use Fabric
for other providers, detached work and sandboxed worktree writers. A native
review must still be independent of the author and satisfy the constitution's
review ladder. Name the goal, paths, constraints, output and verification.

### Native tiers

Select a supported model explicitly when the native tool requires one. Copy
its configured route and effort from tool evidence. Do not infer a resolved
model from an alias, tier or self-report. Junior, senior and principal describe
routing capacity; a reviewer must meet the author's required tier.

## Route pools

```sh
provenant route list
provenant route list --adapter codex
provenant route resolve --route junior
provenant route resolve --route utility
provenant route resolve --model ADAPTER/ID --effort LEVEL
```

The bundled catalogue defines weighted `utility`, `junior`, `senior`,
`principal`, `writing` and `design` pools. The resolver draws among ready
positive-weight members by weight multiplied by provider pacing. `route list`
shows configured weights and pacing policy; dispatch applies the pacing factor
and decides readiness. Zero-weight members are ordered fallbacks. Operation ID
and task index seed the draw so retries repeat the same choice. The utility
pool uses Claude Haiku@low at weight 4, Codex Luna@low at weight 2, then the
two OpenCode `*-free` models at weight 1 each. Free members are ordinary pool members. Omit `--effort` on OpenCode free routes: their
recorded model controls do not support effort, so an explicit value is refused
before launch. Agy Gemini Flash is a paid subscription model and is never
marked free. `--confidential` excludes training or unclassified members and
fallbacks. The Rust Fabric task schema has no per-task
`confidential` field. For an explicit eligibility check, use `provenant route
resolve --route NAME --confidential`.

Named route and exact model selectors are mutually exclusive. An exact effort
is accepted only when the catalogue records that control; unsupported effort
is refused. An explicit exact model can be absent from the catalogue: the
provider decides whether it is available after the normal activation,
disclosure and confinement checks. Named routes and pools require catalogue
entries.
There is no classifier or synonym inference. The bundled Rust catalogue owns
default pool membership and weights; `[domains.routing]` in the state root's
`config.toml` overrides pools and provider pacing. To change routing, run
`provenant route set` or `provenant route pacing`. A Fabric submission carries
one route preference; the task schema rejects unknown fields.

Run `provenant help route` for flags. Named routes and
exact model selectors are mutually exclusive. Explicit fallbacks use
`--fallback ADAPTER/ID,ADAPTER/ID`.
The resolved attempt record owns route provenance. Never claim an unavailable
or substituted provider supplied a missing review leg.

## Task metadata and councils

Rust Fabric accepts one route choice per task; `role` does not select a pool
or resolve a synonym. For a council, submit one task per distinct exact model
and have the chair compare and synthesise the results. The CLI's explicit
fallback models are recovery candidates, not extra council seats. Choose exact
models from the current pool listing when family diversity matters. Ordinary
workspace disclosure follows the configured authority; credentials, secrets
and denied paths stay outside prompts and outputs.

## Version-free family names

Use exact `ADAPTER/ID` identifiers from the current catalogue. Never guess the
actual model behind a provider name.

## Cooldowns

`provenant quota` reads cached observations and `provenant quota tick`
refreshes due accounts. Rust owners decide eligibility.

## Choosing among admissible routes

Choose the simplest eligible route that can meet the task's objective checks.
The [model dossier](../../../docs/model-dossier.md) is advisory; the current
catalogue and config define routes; dispatch and the attempt record decide
availability and provenance. Use explicit effort on the request when required.
Never enable fast or priority service tiers.

## Endpoint profiles

Rust configuration and providers own endpoint validation, named secret
references, argv and environment. `provenant config show` reads effective
configuration. Never put credential values into command lines or receipts.
Unsupported requests return typed refusals rather than silently widening
access. Prefer per-dispatch controls over global provider configuration edits.

## Default stack & fallback chains

Use `fabric_run` (`op: submit`) with an `operation_id` and `tasks`; each task
takes `prompt` (the brief inline), `route`, `timeout_seconds` (1 to 14400), and
optionally `access` and `worktree`. A writer needs a registered linked worktree
and one active writer. Do not launch providers directly to bypass confinement.
The tool schema is the authority for fields and timeout limits.
Rust run owns retries, admission and terminal publication; providers owns wire
mechanics.

## Adaptive review topology

Use independent bounded lanes when their paths and outputs are separable.
The chair keeps coupling, authority and final synthesis. Apply the HARNESS
review ladder, inspect objective evidence and repair concrete defects. A
second pass by the same reviewer does not add independence. Missing family
coverage stays explicit and needs the prescribed replacement lens.

## Diversity caveat

Different endpoint or adapter names do not prove different model families.
Derive family from verified route evidence. Additional advice can inform a
review without filling a required independent review leg.
