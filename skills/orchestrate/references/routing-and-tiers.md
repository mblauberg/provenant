# Routing & tiers

> `config/model-routing.json` is the dated machine catalogue; `scripts/model-route`
> is the policy resolver. This file owns human-readable family/role and
> degradation policy. `HARNESS.md` keeps only the invariant core.
> `docs/model-dossier.md` owns what each model is *like* (strengths,
> weaknesses and cost profile) and the standing model preferences. That dossier
> is advisory and cannot change what is admissible; this file stays the
> authority on task class, tier, role, effort and degradation. Neither restates
> the other.

The resolver's default `--adapter-gate fabric` fails closed when the selected
fabric adapter is disabled or inactive. Runtime Fabric composition separately
requires current provider identity and interface conformance. A direct CLI
executor that owns its own safety and activation gates must opt in explicitly
with `--adapter-gate direct-cli`; this never bypasses explicit denials, adapter
capability, path/write/resource limits or external-action gates. It does not
impose family separation on ordinary execution.

Configured workspace execution is family-agnostic: any configured adapter and
provider family may perform ordinary authorised work when its capability and
scope permit it. Family separation is recorded and enforced only when an
assurance claim requires it. Route receipts retain actual provider/model
lineage so an assurance claim can be checked rather than inferred.

Route every dispatch by **task class, role, evidence surface, safety requirement,
and capability tier**. Never route by a memorised model name. Discover current
model IDs and effort modes at runtime (`cli-headless.md`) and retain the route
receipt.

| Task class | Bound role | Default tier | Default effort | Typical work |
|---|---|---|---|---|
| `mechanical` | worker | scout | low | search, extraction, formatting, deterministic checks |
| `legwork` | worker | workhorse | medium | ordinary implementation, analysis, drafting, source mapping |
| `critical-review` | critical-review | flagship | high | hard review, adversarial verification, design judgement |
| `orchestration` | orchestrator | flagship | high | decomposition, adjudication, synthesis |

`scripts/model-route resolve --task-class ...` is authoritative for these
defaults. An explicit role override may raise effort; an unavailable effort may
substitute only when the receipt records requested and effective values. Alias
routing remains a compatibility surface. Chair inheritance is exceptional: it
must be explicit and recorded, never inferred from an omitted binding.
Task-class dispatch rejects mismatched roles and requires a fresh, adapter-bound
runtime snapshot. Codex snapshots verify model availability and supported effort.
For Agy, the resolver intersects the fresh `agy models` snapshot with the
configured preferred-family alias candidates; the shell owns no second model
catalogue. An unprobed explicit Agy route remains `provider-unverified`, while
an unprobed task-class route fails closed.
Account-default transport omits the literal model, so receipts retain policy
identity. Claude's no-tools, no-session subscription canary verifies the effective
model and fails closed on the CLI's unknown-effort warning, but cannot observe
effort. Task-class dispatch admits only the probed effort, marked
`provider-unverified`; any other effort is rejected. The canary also rejects
caller-authored source labels without scrubbed provenance.
Canaries cost a little; reuse them only within the router's five-minute freshness
window.

## Tiers (relative, family-agnostic)

| Tier | Use for | Reasoning effort |
|---|---|---|
| **scout** | bounded, objective work: extraction, classification, formatting, schema/grep checks, first-pass scouting | low |
| **workhorse** | research legwork, drafting, ordinary review, diff analysis, source mapping | medium |
| **flagship** | sparingly: decomposition, final synthesis, resolving disagreements, hard/high-stakes calls | high |

Current durable aliases (verify against runtime before execution):

| Family | flagship | workhorse | scout |
|---|---|---|---|
| Claude | Opus | Opus, Sonnet | Haiku |
| OpenAI GPT-5.6 | Sol | Terra, Luna | Luna |
| Google Gemini | 3.1 Pro | 3.7 Flash | 3.7 Flash |

Where an alias lists more than one model the order is the resolution order, so
the first is the default and the rest stay admissible. That order is the only
thing that makes a standing preference in `docs/model-dossier.md` take
effect automatically; prose alone does not move it. Change both together or
they will disagree.

Opus is Claude's default flagship and high-effort critical reviewer, and is also
the default workhorse at low or medium effort, where it tends to beat Sonnet at
a higher one. Sonnet stays admissible at workhorse and is the one to reach for
when the work is genuinely routine. Each catalogue-configured risk tier has one bounded
override occupant. Validation prevents it from being an alias or alias
candidate. Lifecycle `risk_tier` remains delivery metadata and never selects
that occupant. Callers must use the separate `--model-override-tier` input,
select the override explicitly and stay within that tier's configured roles,
alias and effort ceiling. The receipt records lifecycle risk and model override
independently. Retargeting a tier removes
that tier's special treatment from its former occupant; a model no configured
tier names is no longer override-only. A malformed override block fails the
whole family closed — every route on that family is rejected with
`risk_tier_config_invalid` rather than quietly leaving its occupant
dispatchable. A family whose `ultra_eligible_roles` is not a list of role names
fails the same way with `effort_policy_config_invalid`, because `in` over a
string silently degrades an eligibility gate into a substring test. A task-class
route whose effort differs from its probe policy's `minimum_effort` fails as
`task_class_config_invalid`: the probe evidences exactly one effort, so a
divergence is a configuration error and must not surface as the provider fault
`effort_capability_unverified`. Fable currently occupies both configured tiers. Sol leads for
Codex. Eligible Sol lead/orchestrator routes may use Ultra; runtime model
capabilities decide the effective effort and every fallback is recorded. Claude
and Codex are equal primary families.

Effort rule: **medium by default**; **high for verification, adversarial, and high-stakes** calls
(that's where subtle errors hide); reserve the very highest effort for isolated single-shot calls —
it can be slow and has been observed to hang inside agent loops, so don't run it in a tight loop.

Cost is not just tokens: tools meter differently (tokens vs credits vs monthly caps), and the highest
tiers cost far more per call. A "small, objective" task only stays cheap if the schema is strict and
the output is validated — a loose schema lets a cheap model invent fields, which costs more in rework.

## Choosing among admissible routes

The tables above decide what a route *must* be. When more than one route
satisfies them, read `docs/model-dossier.md`. It is the single advisory
document: it records the standing preferences in plain prose, is edited directly
by the operator, and is enforced by nothing. Prefer what it says, depart from it
when the work calls for something else, and record why. Then choose on model
character rather than habit, from the same file's per-model strengths,
weaknesses and cost profile, and its category notes for adversarial,
long-context, cheap-bulk and effort-substitution work. Its entries are examples,
not an enumeration, so a task whose character is unlisted is still routed by
reasoning from the nearest entries. The dossier is advisory: it ranks
admissible options and never widens authority, reaches a disabled adapter, or
overrides a reservation, tier or compatibility gate.

When a dossier entry actually decided between two admissible routes, name that
entry's heading in the run receipt and the worker brief, for example
`dossier: GPT-5.6 Sol`. Record nothing when no entry informed the choice; an
unapplied preference is not evidence the dossier helped. The resolver receipt
from `scripts/model-route` has a fixed schema with no advisory field, so the
citation lives in the chair-authored run receipt and worker brief, never in
resolver output.

## Endpoint profiles

`config/model-routing.json` carries an `endpoints` map of Anthropic-compatible
provider endpoints (Z.ai GLM, Moonshot Kimi and DeepSeek ship as examples). Each
profile names a base URL, the model family it serves, the adapters allowed to use
it, and the environment variable that holds the token. Tokens are never stored in
the catalogue or written into a route record. Name one with
`CF_DISPATCH_ENDPOINT=<profile>` and pass an explicit `--model`: the resolver
takes the profile's family in place of the adapter's pinned family, emits
`endpoint_base_url` and `endpoint_token_env`, and the dispatcher exports
`ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` into the Claude child process
only. These endpoints expose no reasoning-effort control, so an endpoint route
carries no effort at all and rejects an explicit `--effort` rather than claiming
one. A route naming an unknown profile, an adapter the profile does not list, or
a token variable that is unset fails closed with a typed status and dispatches
nothing.

A profile listing `codex` among its adapters reaches an OpenAI-compatible
endpoint instead, and must also declare `wire_api` (Codex 0.146 accepts only
`responses`). Codex keeps `--ignore-user-config` on every route, so the provider
is passed inline as `-c model_providers.provenant_endpoint.*` overrides plus
`-c model_provider=provenant_endpoint` rather than read from the operator's own
`config.toml`. The token stays named: `env_key` tells Codex which variable to
read, so no credential reaches an argument vector. See
[ADR 0023](../../../docs/adr/0023-codex-custom-providers-inline-config.md).

## Default stack & fallback chains

Default once this skill triggers: **native same-harness workers + at least one different-family
verifier/adversary** when a safe, data-authorised route exists. For high-stakes or low-oracle work, use
two different-family passes where practical. Prefer the safest adapter that can inspect the needed
artifact. If no safe external adapter is available under the host data policy, use objective local
checks and record `CROSS-FAMILY-NOT-RUN` instead of pretending it happened.

In dynamic workflows, bind every stage from its task class. Route bulk
scan/extract stages to scout and reserve flagship for synthesis/adjudication
(`dynamic-workflows.md`).

## Adaptive review topology

Review pressure follows the current `HARNESS.md` ladder. Substantial work uses
multiple targeted lenses and one strong other-primary review. Targeted lenses
may use smaller models; their briefs must differ by failure surface. Crucial work
uses a distinct family when available. Terminal work adds stronger targeted and
adversarial pressure, and any skipped distinct-family leg records its reason.

The chair schedules these legs under a per-run configurable concurrency ceiling
and may sequence them around deterministic checks. It need not wait for a
particular model. A missing distinct-family leg requires an omitted-leg reason;
the other-primary leg remains required at substantial and above. Overlap creates
defect pressure, not votes; objective checks and source evidence remain
authoritative.

Express chains by **role → tier/family**, resolving names at runtime:

```
verify      → safest different-family read-only adapter → objective checks → user if needed
adversary   → strong different-family critic → source/test-backed fix list
long-ctx    → long-context scout → file-backed synthesis → flagship decision
bulk/scout  → cheap diverse scout → strict schema → sampled verification
```

On an auth/quota/limit/safety error from a tool: log it to the run scratchpad and advance to the next
entry. Never silently skip the verification step.

Cursor, Copilot, Kiro, OpenCode, Agy and Pi are adapters, not model families.
Record the actual provider/model lineage. Kiro and OpenCode execution are
currently disabled by compatibility policy even though their Fabric MCP client
registrations remain supported. Gemini, xAI and other distinct families are
flexible advisory workers/reviewers: useful for blind spots, never load-bearing
when quota/API output is absent. Pi stays dormant until a pinned distinct
open-model route, current Herdr integration and smoke evaluation exist; it may
not broker Claude/OpenAI and claim distinct-family certification.

Provider-subscription preferences are deliberately small and explicit. Agy is
Gemini-first and optional/advisory until its Fabric route has repeatable
subscription-backed evidence. Cursor prefers xAI/Grok first, then Composer.
If neither is available, route an explicit recorded fallback through the
family's native Claude, Codex or Agy adapter; do not rebroadcast that family
through Cursor unless its current compatibility contract admits it. The
ordered machine policy is in `config/model-routing.json`; it never authorises
automatic retries or silent substitution.

## Diversity caveat

Frontier families increasingly make *correlated* errors, so "ask another model" is weakening on its
own. Lean on **objective/locally-checkable verification** and (in non-code domains) source-anchoring +
action-authority gates, with cross-family review as pressure on top — not as the sole safety net.
