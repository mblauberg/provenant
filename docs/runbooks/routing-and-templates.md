# Routing and templates
Owner: provenant-routing
Anchor: [`resolve.rs`](../../tools/front-door/crates/routing/src/resolve.rs); [ADR 0039](../adr/0039-graph-of-templates.md#interface)

How to choose and steer routes, pace accounts and shape review panels. The
bundled catalogue (`tools/front-door/crates/routing/assets/catalogue.toml`) owns
default pool membership, weights and effort controls; the
[routing spec](../specs/routing.md) owns the rules. Everything below is
overridable in the selected state's `config.toml`.

## Inspect and choose

- `provenant route list [--adapter A] [--pool P] [--valid]` shows pools, weights
  and pacing; `--valid` lists model/effort pairs resolution accepts without
  probing availability. Readiness is decided at dispatch, so read the resolved
  `adapter/model@effort` from the attempt record and never infer a model from
  a tier name.
- `provenant route resolve NAME` (or `--route`, `--model ADAPTER/ID --effort E`,
  `--exclude`, `--account`, `--confidential`) previews a choice, its pacing
  factors, exclusions and the governing config source.
- Edit pools with `route set POOL MODEL [--weight N] [--effort E]`,
  `route remove POOL MODEL`, `route reset POOL`; provider pacing with
  `route pacing PROVIDER pace|spend|max|off`.
- Default bounded work to `provenant run --tier junior --file BRIEF --explain`.
  An unlisted exact model ID warns and is tried unverified; an explicit
  `--effort` on it is refused. OpenCode free routes take no effort.

## Pacing

The resolver draws among ready members by weight times the pacing factor. `pace`
divides remaining quota headroom by remaining window time (clamped 0.25 to 2.0)
and takes the lowest factor across windows with a future reset; a window past
its reserve (defaults 85% used for five-hour, 90% for seven-day) makes the
account unavailable as `pacing_reserve`. Missing quota data is neutral. Go pool
members with uncalibrated partial windows are ineligible unless
`routing.allow_unknown_partial_headroom` is true. Per-account profiles beat
provider defaults; a profile can be `spend`, `pace`, `max` or `off` (`off`
prevents routing, free models included):

```toml
[domains.routing.pacing_reserve.claude]
five_hour_percent = 15

[domains.routing.pacing.accounts.michael]
policy = "pace"
windows = ["seven_day"]                       # five_hour, seven_day, monthly, rolling, daily
deadline = { reset_of = "anese" }             # or "thu 00:00", "day 25", "2026-11-10T09:00"
after_deadline = "standby"                    # weight zero while another member can serve
```

Account keys accept registered keys, entry IDs or labels (`adapter/key` to
disambiguate). An unknown or ambiguous reference blocks resolution for every
provider: remove stale profile and `reset_of` references when unregistering.

Temporary changes: `provenant route override set --all --policy spend --until 1d`
(or `--account`, `--provider`; `--window mon-fri@23-7 --tz ZONE`), then `route
override list` and `route override clear`. Each scope holds one override. Keep a provider, account or exact model out of named pools with
`route exclude add codex/gpt-6-luna --until 2h`, `route exclude list` and
`route exclude remove`; standing exclusions use `routing.exclude` and
`routing.graph_exclude`. An exclusion never widens a pool, and an explicit
`--model` pin ignores it.

## Model policy

Bundled policy (`crates/routing/assets/model-policy.toml`) supplies compaction
and cache advice, lane limits and enforced result checks. Override under
`[domains.routing.model_policy]`: layers run bundled, user defaults, then
matching `rules` by scope (global, adapter, model, account, model plus account),
later rules winning, with optional `schedule` (`starts`, `expires` as quoted
RFC 3339, `tz`, hour `windows`). `model_suffix` matches the end of a native ID.
`provenant route resolve MODEL [--account KEY]` shows the effective policy and
its sources. Malformed rules are dropped with a warning, or refuse configuration
when they carry an enforced restriction (`read_only`, `max_context`,
`max_turns`). Retired timing keys (`starts_unix_seconds`, `expires_unix_seconds`,
top-level `windows`) block every configuration write until
`provenant config migrate` rewrites them.

`max_context`, `max_turns` and cache keys are guidance, not kills.
`lane.max_continuations` (default 2, maximum 8) bounds automatic restarts of a
writer that declares `stopped_early`; `blocked` returns to the chair.
`lanes.context_handoff_tokens` (off; 200000 suggested) continues a Claude writer
in a fresh attempt on the same worktree when one call's context reaches it.

## Review panels

Shapes group read-only reviewers by pool and count (bundled `quick`, `standard`,
`deep`); `distinct_families` is a preference that records a substitution. Select
shapes by risk, kind, size or path:

```toml
[domains.routing.review.shapes.deep]
reviewers = [{ pool = "junior", count = 4, distinct_families = true },
             { pool = "senior", count = 4, distinct_families = true }]
[domains.routing.review.select]
default = "standard"
by_risk = { routine = "quick", crucial = "deep", terminal = "deep" }
by_paths = { ".github/workflows/**" = "deep" }
[domains.routing.review.lenses.by_kind.docs]
order = ["conventions-readability"]
```

Size bands are `domains.routing.review.size`. A template review of a `head` or
`node` subject with no `size` or `review_shape` is deferred: the host measures
the pinned commit when the review starts. Risk tags (`security`, `concurrency`,
`terminal`) add conditional lenses; set the `risk_tags` parameter, since a risk
tier alone does not. `[review.minimum_family_counts]` in a run document is
advisory unless `review.diversity = "strict"`.

```sh
provenant run --use review --file brief.md --set subject='"origin/branch"' --set review_base='"origin/main"' --explain
```

The bundled panel is one `review-shape` composite. The host binds the launch
values `review-frame`, `review-verify` and `review-synthesis` (`true` or
`false`) from the selected shape, and the framing, verification and synthesis
stages carry `when` on them. A stage that is off shows as skipped in `--explain`
and `status`; there is one template to override for a custom panel, not one per
combination.

Submit without `--explain` and bind each attempt's resolved route to the review
evidence. A changed head needs delta review.

## Templates

`provenant templates [NAME]` lists and shows templates; that is the reference.
Use `task` for one assignment, `plan` to shape work, `implement`, `review`,
`batch` (disjoint slices into one integration branch) and `deliver`
(implementation through landing; grants no publication authority).
Register the writer worktree first (`provenant worktree create delivery --base
BASE`) or pass `--branch NAME --base REF` to `run`, then bind parameters in a
reusable document:

```toml
use = "deliver"
[with]
worktree = "/absolute/project/.worktrees/delivery"
write_scope = ["src"]
brief = "Meet the approved acceptance criteria and run the named checks."
```

`provenant run --use deliver --file delivery.toml --explain`, inspect authority
and gates, then submit without `--explain`. `--wait` only bounds observation.
Binding a writer may block longer than a short `--wait` (the allowance is
`domains.fabric.graph.writer_launch_seconds`, default 55); a busy registry
returns `writer_lock_busy`.

- Repository templates live in `.provenant/templates/`; layers are bundled,
  user, repository, inline. A leaf template has a brief, tier, access and
  outputs; a composite has `nodes` (`use`, `after`, `over`, `min_ok`, `brief`,
  `tier`). Dotted names such as `review.security` are flavours.
- `when = { name = value }` on a node, or on an entry of a template's
  `exports`, runs it only if every named value matches. A name is a parameter of
  the enclosing template or a launch value the host binds as a single value; the
  match is decided at submission, so a mistyped name is refused with the name
  and the nearest valid ones. A node that does not match stays in the graph as a
  skipped row that names its condition (`status`, and `skipped` in `--explain`).
  Nodes that depended on it run as if it were absent, and a node that needs its
  output (`source`, `continues`, `land`, `on_fail`) is refused. `when` is not
  supported on a loop or a `repeat`.
- Identity: `identity = "fresh"` (`task`) starts a new graph every time;
  `"content"` (`review`, `deliver`) reuses an equal submission at the same Git
  HEAD, so a new commit starts a new review. Use a new `operation_id` for new
  work.
- `light` (`--set 'attempt.preset="light"'`) trims read-only lanes (shared source
  view, minimal supervision, no receipts); writers refuse `shared` isolation.
- Repair: `deliver` and `batch` hand only confirmed findings to a repair writer
  (`max_repair_cycles`, default 3); a third round asks the principal for design
  judgement rather than ending the work.
- Inside a worker, `run --use TEMPLATE` adds children to the same graph.
  `edit GRAPH --file PATCH` reshapes unstarted nodes only. Observe with
  `status --wait=5m --target graph:GRAPH`, `output GRAPH --trace`, `stop`,
  `retry`. A synthesis never waives a failed gate; land only after exact-head CI
  and required review.
