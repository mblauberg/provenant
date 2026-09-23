# Routing and tiers

The dated source is `config/model-routing.json`; `scripts/model-route snapshot --json` merges the product and per-key user overlay. Read the effective catalogue and health through `fabric_adapters`. `docs/model-dossier.md` explains model strengths and cost, without changing admissibility.

For ordinary provider work, use `fabric_dispatch`: one top-level task or `tasks[]` (1–64, concurrency at most 8). Pass `prompt` or `prompt_file`, optional `adapter`, `model` or `alias`, `effort`, `mode`, `worktree`, and per-run controls. A model shorthand such as `luna`, `sol`, `astra` or `opus` resolves to its owner. A non-tier `alias` is treated as a model with a note. An explicit model wins over a conflicting alias with a note. Unknown models pass through when the provider can run them. Unsupported effort moves to the nearest supported value and the receipt records requested and applied values.

Writes require an owned registered worktree. Only impossible execution or a hard boundary fails preflight: missing prompt, missing CLI, unowned writer worktree, credential-store exposure or write sandbox on a read-only run. Read-only guarantees vary by adapter and appear in the receipt; `best_effort` and `prompt_only` must not be claimed as enforced.

Take the returned run id and call `fabric_status` with `ids: [id]` and `wait_seconds: 55`. `fabric_output` gives bounded live tails. `fabric_cancel` stops the process group. A worker question yields `input_required`; reply with `fabric_dispatch` using `resume: id` and a new prompt. For one task of a batch, pass the run id plus `task_id`, or the task's own id. Terminal digests include the ready-to-paste provenance line and, when known, the session size (`ctx 212k/1M`). Resuming a large session re-reads its whole context. When the digest warns `! resuming a ~620k-token session`, prefer `fabric_dispatch` with `handoff: id` and a new prompt. It starts a fresh session primed with the prior route and result tail. `context_ceiling` (default 300,000 tokens) can lower Claude and Codex auto-compaction but never raises it. Where the provider already compacts earlier, or its point is unknown, the receipt records `provider_default`. Other adapters record `unsupported`. Direct CLI is a degraded path under [direct-cli-fallback.md](direct-cli-fallback.md).

Task class selects `flagship`, `workhorse` or `scout` when no explicit model is chosen. The configured catalogue determines candidates; the receipt is authoritative for the applied route. A cooling explicit model still runs with a warning; alias routes skip cooling candidates. An account-level usage limit cools the whole adapter, except on agy, which meters each hosted model separately. Automatic fallback stays within permitted paid non-training routes unless the caller opts into `fallback: "any"` or an explicit list.

## Tiers (relative, family-agnostic)

| Tier | Use for | Reasoning effort |
|---|---|---|
| **scout** | bounded, objective work: extraction, classification, formatting, schema/grep checks, first-pass scouting | low |
| **workhorse** | research legwork, drafting, ordinary review, diff analysis, source mapping | medium |
| **flagship** | sparingly: decomposition, final synthesis, resolving disagreements, hard/high-stakes calls | high |

Concrete alias candidates and their resolution order live only in
`config/model-routing.json`; verify them against runtime before execution.
The first configured candidate is the default and later candidates remain
admissible. `docs/model-dossier.md` records advisory preferences, so prose
alone does not move a default.

Opus (the `opus` alias, which resolves to Opus 5.5, `claude-opus-5-5`) is
Claude's default flagship and the standing choice for critical review, synthesis
and adjudication at every risk tier. It is also the default workhorse at low or
medium effort, where it tends to beat Sonnet at a higher one. Sonnet stays admissible at workhorse and is the one to reach for
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
`effort_capability_unverified`. Claude Fable 5.1 (`claude-fable-5-1`) currently
occupies both configured tiers. The override is opt-in and is not the default
for crucial or terminal work: prefer Opus 5.5 at `high` or `xhigh`, and select
Fable only when a deliberately different Anthropic mind is wanted or the owner
asks for it.
Astra leads for Codex and is the only OpenAI flagship candidate, so a worker
model is never a silent flagship fallback. The standing policy runs Astra between `low`
and `xhigh` for critical review and for legwork that needs judgement. The
native Codex CLI reports `max` and `ultra` for Astra and the Responses API
stops at `max`; those are separate surfaces, only the runtime capability probe
decides what the adapter can dispatch, and the catalogue defaults to neither.
Every substitution is recorded. The workhorse alias lists GPT-6 Sol
(`gpt-6-sol`) first, with GPT-6 Luna (`gpt-6-luna`) as its admissible
fallback; scout is Luna. Both run at `high` by default and are raised to
`xhigh` or `max` when a slice warrants it. GPT-5.6 models and Terra are no
longer catalogue routes. Claude and
Codex are equal primary families.

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
`dossier: GPT-6 Astra`. Record nothing when no entry informed the choice; an
unapplied preference is not evidence the dossier helped. The resolver receipt
from `scripts/model-route` has a fixed schema with no advisory field, so the
citation lives in the chair-authored run receipt and worker brief, never in
resolver output.

## Endpoint profiles

`config/model-routing.json` carries an `endpoints` map of Anthropic- or
OpenAI-compatible provider endpoints (Z.ai GLM, Moonshot Kimi, DeepSeek and
OpenRouter ship as examples). Each
profile names a base URL, the model family it serves, the adapters allowed to use
it, and the environment variable that holds the token. Tokens are never stored in
the catalogue or written into a route record. Name one with
`CF_DISPATCH_ENDPOINT=<profile>` and pass an explicit `--model`: the resolver
takes the profile's family in place of the adapter's pinned family, emits
`endpoint_base_url` and `endpoint_token_env`, and the dispatcher exports
`ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` into the Claude child process
only (and blanks `ANTHROPIC_API_KEY` so a gateway cannot fall back to Anthropic
direct auth). These endpoints expose no reasoning-effort control, so an endpoint route
carries no effort at all and rejects an explicit `--effort` rather than claiming
one. A route naming an unknown profile, an adapter the profile does not list, or
a token variable that is unset fails closed with a typed status and dispatches
nothing.

OpenRouter is a multi-model gateway: use `openrouter-anthropic` with Claude or
`openrouter-openai` with Codex (`wire_api: responses`), set `OPENROUTER_API_KEY`
in the environment, and pass the live slug (for example `stealth/union-alpha` or
`moonshotai/kimi-k3`). Receipts record upstream `model_family` when the slug
infers one (`family_source: slug-inferred`); stealth/unknown broker ids stay
`generic-open` and are not distinct-family eligible. Do not pin rotating free or
stealth models into alias tables; pick the slug at dispatch time.

OpenCode is an ordinary implemented broker for its catalogue (`opencode/<model>`).
It defaults to `opencode-go/deepseek-v4.1-flash`; `opencode-go/glm-5.3-flash`,
`opencode/mimo-v2.6-flash-free` and `opencode/muse-spark-1.3-contributor-free`
are the other preferred models (Muse may train on prompts: never send it sensitive
content). Use `opencode models` to discover a live slug when overriding with `--model`.
OpenCode resolves workspace file reads from the worker's current directory. If
the prompt names supporting files elsewhere in the workspace, set Fabric's
read-only `cwd` to a directory that contains them; `prompt_file` supplies the
task text but does not stage those referenced files.
Nested vendor ids attribute as that vendor; unparseable Zen free ids fall back
to `generic-open` (worker only, not assurance). See
[ADR 0025](../../../docs/adr/0025-broker-upstream-family-attribution.md).

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
Record the actual provider/model lineage. Kiro execution remains disabled by
compatibility policy even though its Fabric MCP client registration is
supported. OpenCode execution is enabled when the `opencode` CLI is installed:
its free default applies unless `--model` selects a live slug (discover with
`opencode models`).
Upstream family on the receipt follows the slug when knowable; otherwise
`generic-open` (ordinary worker, not distinct-family assurance).
Gemini, xAI and other distinct families are
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
