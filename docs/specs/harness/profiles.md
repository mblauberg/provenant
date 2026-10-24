# Spec: harness profiles for cost and quality trade-offs

Status: **proposed**. GitHub owns delivery state; this file owns the durable
requirements. It grants no authority beyond what the user approves by
accepting it, and it does not relax any gate in `HARNESS.md`.

## Outcome

The user can try a variation of the harness, such as cheaper reasoning on
legwork, narrower fan-out or a stricter review posture, by switching
configuration rather than editing skills or scripts. Each variation can be
benchmarked on real work, Sealle included, because every attempt records
which combination produced it.

- Every cost-versus-quality choice the harness hard-codes is listed below,
  with its location, current value and trade-off.
- The choices that are safe to vary are named knobs in one registry,
  [`config/harness-knobs.json`](../../../config/harness-knobs.json). A knob's
  default is today's behaviour, so nothing changes until the user switches.
- Presets name whole postures: `economy`, `balanced` (today) and `rigorous`.
- Safety floors are not knobs.
- Every dispatch attempt carries a profile stamp, so outcomes, tokens and cost
  compare per combination.

## Inventory

Status `wired` means code or a skill reads the knob; `advisory` means the
registry records the choice, `provenant profile show --resolved` reports it,
and the owning prose or constant still holds today's value until it is wired.
`fixed` means a safety floor or user policy, deliberately not a knob.

| Choice | Where | Current value | Trade-off | Knob / status |
|---|---|---|---|---|
| Effort of each task-class route | `config/model-routing.json` `task_class_routes.*.effort`, floored by each route's `minimum_effort` | mechanical/bulk `low`; legwork/research `medium`; the rest `high` | Reasoning tokens against depth | `effort.shift`, wired |
| Batch fan-out width | `skills/orchestrate/scripts/batch_run.py` `MAX_CONCURRENCY` 8, `DEFAULT_CONCURRENCY` 4; Fabric batch 4, councils 8 (`runtime/fabric/src/execution.ts`) | up to 8 lanes | Quota and memory pacing against wall time | `lanes.max_concurrency`, wired |
| Silent-provider re-check | `skills/orchestrate/scripts/provider_exec.py`, abandoned attempts in `dispatch_run.py` | 300 seconds (1–3600) | Earlier observation against filesystem sampling cost; never kills for silence | `lanes.silence_seconds`, wired |
| Auto-compaction ceiling | `config/model-routing.json` `context.ceiling_tokens`; `context_usage.py` clamps 100k–1M | 300,000 | Re-read tokens per turn against loss of detail | `context.ceiling_tokens`, wired |
| Reviewers above the ladder | `HARNESS.md` Review ladder; `skills/code-review/SKILL.md` step 5; `references/multi-agent-review.md` (one reviewer at substantial, councils 2–4) | ladder minimum | Reviewer lanes against defect coverage | `review.extra_reviewers`, wired (skill) |
| Cross-family requirement | `HARNESS.md` ladder: substantial falls back to same-family with another lens | prefer | Decorrelation against waiting on quota | `review.cross_family`, wired (skill) |
| Re-review after repair | `skills/implement/SKILL.md` step 5 | repair delta only | Interaction defects against a second full review | `review.rereview_scope`, wired (skill) |
| Repair cycle stop | `skills/deliver/references/contract.md` | 2 / 4 / 5 / 5 by tier | User attention against repair spend | `repair.max_cycles`, advisory; the ceiling is fixed |
| Council juniors per part | `skills/orchestrate/references/tiered-council.md` | 3 | Lens passes against family spread | `council.junior_size`, advisory |
| Pool pick mode | `scripts/model_route_pools.py` `pick` (`top`, `rotate`, `council`) | top weight | Consistency against quota spread | `routing.pool_mode`, wired |
| Final voice pass on prose | `skills/engineering-writing/SKILL.md` step 6 | optional | One writing lane against voice | `writing.final_polish`, advisory |
| Worker summary size | `skills/orchestrate/references/layering-and-context.md` | about 1–2k tokens | Chair context against inline detail | `context.summary_tokens`, advisory |
| Quota pacing tolerance | `scripts/provider_quota.py` `PACE_TOLERANCE` | 4 points | Early pacing against bursts | `quota.pace_tolerance`, advisory |
| Pool membership and weights | `config/model-routing.json` `routes.*` | per pool | Model cost against quality | not a knob: edit the catalogue or `~/.agents/config/model-routing.json` overlay |
| Task-class model affinity | `~/.agents/config/model-preferences.json` | per user | Family and model choice | not a knob: already user configuration |
| Review ladder minimums | `HARNESS.md`; `crates/workflow` coverage | routine 0, substantial 1, crucial 2 (two families), terminal +adversarial | Safety | fixed |
| Task-class routing and effort-shift minimums | `config/model-routing.json` `task_class_routes` | per class | Routing | configurable; `effort.shift` never steps below `minimum_effort` |
| Repair validator ceiling | `skills/deliver/references/contract.md` | 2 / 4 / 5 / 5 | Safety | fixed |
| Credential and path rules, CI merge gating | `HARNESS.md`, `AGENTS.md` | as written | Safety | fixed |
| Local verification depth | user `CLAUDE.md`: heavy checks in CI | targeted checks locally | User policy | fixed |
| Lane preface | `skills/orchestrate/scripts/provider_exec.py` | about 70 tokens | Attribution guard | fixed |

On the shipped catalogue every task-class route already sits at its policy
minimum effort, so `effort.shift: -1` saves nothing until a route is
configured above its minimum. `+1` takes effect at once.

## Registry

Each knob in `config/harness-knobs.json` has:

- `type`: `int` (with `min`, `max` and optional `nullable`), `enum` (with
  `values`, least rigorous first) or `bool`;
- `default`: today's behaviour;
- `status`: `wired` or `advisory`;
- `reader`: the file that consumes or owns the choice;
- `effect` and `trade_off`: one line each;
- optional `floor`: a map from risk tier to the minimum value at that tier
  and every tier above it, in `config/risk-policy.json` `tier_order`. Enum
  floors compare by position in `values`.

Unknown knobs, unknown fields, out-of-range values, unknown presets and
unknown task classes are errors. Task classes are the keys of
`task_class_routes` in `config/model-routing.json`.

## Presets

A preset is a partial knob map with optional per-task-class maps. `balanced`
must be empty.

| Knob | economy | balanced | rigorous |
|---|---|---|---|
| `effort.shift` | 0 | 0 | +1 (0 for mechanical and bulk) |
| `lanes.max_concurrency` | 4 | 8 | 8 |
| `context.ceiling_tokens` | 200,000 | catalogue | catalogue |
| `review.extra_reviewers` | 0 | 0 | 1 |
| `review.cross_family` | prefer | prefer | require |
| `review.rereview_scope` | delta | delta | full |
| `council.junior_size` | 2 | 3 | 4 |
| `routing.pool_mode` | rotate | top | top |
| `writing.final_polish` | off | optional | optional |
| `context.summary_tokens` | 1,000 | 2,000 | 2,000 |

## Resolution

One module, `scripts/harness_profile.py`,
resolves profiles for every Python reader and for the CLI. Fabric's
TypeScript runtime does not resolve profiles; it reaches them through the
Python owners it delegates to.

Lowest to highest:

1. registry defaults;
2. the preset;
3. the user file, `<instance root>/config/harness-profile.json`
   (`~/.agents/config/harness-profile.json` by default);
4. the project file, `.agents/harness-profile.json` at the checkout root, so
   Sealle and Provenant can differ;
5. the environment: `PROVENANT_PROFILE` names a preset and
   `PROVENANT_KNOB_<NAME>` sets a knob, with the name upper-cased and every
   non-alphanumeric character replaced by `_`;
6. per-dispatch overrides: `provenant dispatch --knob NAME=VALUE`, repeatable.

The preset applied is the one named by the highest layer that names any.
Within a layer, an entry under `task_classes.<class>` beats the layer's
general entry. A floor for the dispatch's risk tier is applied last and wins
over every layer.

A writer lane can edit its checkout, so the project file may only strengthen
a knob the registry marks `"project": "strengthen"` (every `review.*` knob).
For those knobs the resolver compares the project value with the value the
other layers give and keeps the stronger by registry rank; the source then
reads `... (project may only strengthen)`. The user file, the environment and
`--knob` may still lower them.

The user file is found through `PROVENANT_PROFILE_ROOT`, then
`AGENT_FABRIC_INSTANCE_ROOT`, then `~/.agents`. When `dispatch_run.py` points
routing at the product catalogue because the instance root has no
`config/model-routing.json`, it pins `PROVENANT_PROFILE_ROOT` to the original
instance root first, so routing and the receipt read the same user file.

A profile file has the shape:

```json
{"schema_version": 1, "preset": "economy",
 "knobs": {"lanes.max_concurrency": 2},
 "task_classes": {"critical-review": {"preset": "rigorous"}}}
```

Runtime readers are lenient: a profile that does not validate leaves the
lane on today's behaviour and the stamp records the error. The CLI and an
explicit `--knob` are strict and exit with an error instead.

## Wiring

- `effort.shift` (`scripts/model_route.py`): after a task-class route is
  chosen, its effort moves one step along `low`, `medium`, `high`, `xhigh`
  (never `ultra`; `max` is retired and rejected), only when the new effort
  meets the class's configured `minimum_effort` (defaulting to its route effort)
  and every configured model lists it; otherwise the route keeps its effort. The route
  record carries `effort_shift`. A runtime fallback may not go below the lower
  of the configured and shifted effort.
- `lanes.max_concurrency` (`batch_run.py`): caps the requested concurrency for
  both `provenant batch` and Fabric batches; it never raises it.
- `lanes.silence_seconds` (`dispatch_run.py`, `provider_exec.py`): interval
  without stdout or stderr before retaining `watchdog.json`; default 300,
  range 1–3600. The reaper and status readers use the attempt's saved knob.
  Silence never signals a provider; startup and wall deadlines still apply.
- `context.ceiling_tokens` (`dispatch_run.py`): the ceiling for a dispatch that
  passes no `--context-ceiling`. An explicit flag and a resumed run's ceiling
  still win.
- `review.*` (`skills/code-review/SKILL.md`, `skills/implement/SKILL.md`): one
  line each tells the chair to read `provenant profile show --resolved`. The
  ladder in `HARNESS.md` stays the floor.

`dispatch_run.py` exports `--knob` values and a recognised `--risk-tier` to the
environment, so the routing subprocesses resolve the same profile. `--knob`
values are forwarded when a lane is placed on a peer host.

## Telemetry stamp

Every `fabric.attempt.v1` receipt carries a top-level `profile` object:

```json
{"preset": "economy", "digest": "e6b50d77ebbe", "task_class": "legwork",
 "risk_tier": null, "knobs": {"effort.shift": -1, "...": "..."},
 "floored": ["effort.shift"]}
```

`digest` is the first 12 hex digits of the SHA-256 of the canonical JSON of
the knobs that differ from their defaults. Equal behaviour gives an equal
digest however it was reached, `balanced` always has the same digest, and
adding a knob at its default changes no existing digest. A profile that
failed to resolve stamps `preset` and `digest` as `null` with an `error`.

The telemetry ledger already ingests attempt receipts. Carrying the preset and
digest into ledger rows and `provenant stats` grouping is a follow-up once the
telemetry slice in PR #1056 lands, so this change does not touch ledger code.

## Comparing combinations

An A/B holds the work constant and varies one profile:

1. Pick a bounded, repeatable slice of work (a replay bench task from issue
   #1064, or a run of similar Sealle issues) and a risk tier.
2. Run it under the first profile, for example `PROVENANT_PROFILE=balanced`,
   then under the second, in the same checkout and on the same models.
3. Group attempt receipts by `profile.digest` and compare status, retries,
   review findings, tokens, cost and wall time. Treat a side with fewer than
   ten attempts as inconclusive.
4. Keep a combination by writing it to the user or project file; record the
   decision and its evidence in the issue that motivated it.

## Acceptance

- With no profile file and no variables, every knob resolves to its default
  and routing, fan-out, compaction and review behave exactly as before.
- `provenant profile list|show|use|set|reset|diff` validate against the
  registry, and unknown knobs are errors.
- Precedence, task-class overrides, environment parsing and floors are covered
  by `tests/test_harness_profile.py`, as are the seven wired readers and the
  stamp. `routing.pool_mode` is wired for #1119 so profiles can select rotation.
- Every attempt receipt carries `profile.preset` and `profile.digest`.
