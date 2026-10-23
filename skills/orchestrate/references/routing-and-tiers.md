# Routing and tiers

The dated source is `config/model-routing.json`; `scripts/model-route snapshot --json` merges the product and per-key user overlay. Read the effective catalogue and health through `fabric_adapters`. `docs/model-dossier.md` explains model strengths and cost, without changing admissibility.

## Native first

Each primary runs its own models as native subagents: Claude Code through the Agent tool with `opus`, `sonnet` or `haiku` (the 5.5 models), Codex through its native subagents. Fabric carries other providers, agy's Claude 4.6 models included, long or detached runs and sandboxed worktree writers. Fabric enforces this lightly for a known Claude or Codex seat (`AGENT_FABRIC_SEAT`); an unknown seat changes nothing:

- a pool pick (default, `rotate`), a dispatch with no selector and a tier alias skip the seat's own models, with `<adapter>/<model> skipped: use a native subagent`;
- `council: N` hands native members back as `spawn N native <adapter> member(s) (<models>); Fabric runs m of N`;
- before this check, empty selectors are dropped, mixed selectors settle by precedence, and a tier alias is read case- and typo-tolerantly (`Senior`);
- when nothing else is available, the pick is rejected with `route_native_only`, whose fix names the native spawn and `adapter: <seat>`: the seat's own models never run by default;
- an explicit `adapter`, `model` or `models` entry naming the seat's own model is rejected as `native_seat_model` on a Claude seat (use the host's native subagents); a Codex seat runs it with a leading `NATIVE:` warning.

### Native tiers

The [tier pools](#route-pools) span every adapter, Claude included, but a Claude seat runs its own pool members as native subagents rather than through Fabric. Give the Agent tool the Claude member of the work's tier pool (`config/model-routing.json` `routes`), not the session's model:

| Tier | Claude member (Agent tool) | Native work |
|---|---|---|
| Junior | `haiku` | search, extraction, transcript and log sweeps, lens passes |
| Senior | `sonnet` | implementation, review legs, research synthesis, medium decisions |
| Principal | `opus` | critical review, decisions, orchestration, and the final design or taste call |

Opus is not a default for search, implementation or ordinary review: in recorded sessions, subagents spent about three times the chair's tokens and Opus subagents were the largest line. Escalate a lane to `opus` when its task class is principal, not because the chair runs on Opus.

A native subagent's context is re-read on every turn. Brief it with paths and let it read; do not paste files, diffs or transcripts into the prompt. End it once its deliverable lands. Continue it only for follow-up on that same deliverable; unrelated work goes to a fresh subagent.

For ordinary provider work, use `fabric_dispatch`: one top-level task or `tasks[]` (1–64, concurrency at most 8). Pass `prompt` or `prompt_file`, optional `adapter`, `model` or `alias`, `effort`, `mode`, `worktree`, and per-run controls. A model shorthand such as `luna`, `sol`, `astra` or `opus` resolves to its owner. A non-tier `alias` is treated as a model with a note. An explicit model wins over a conflicting alias with a note. With no `adapter`, `model` also takes `adapter/id` or `adapter/id@effort` (a named `effort` wins); `opencode/opencode-go/<id>` keeps its provider path. An id the catalogue does not register runs on its adapter with a note. `fabric_adapters` with `models: <adapter>` and optional `match` (CLI `fabric adapters --models <adapter> [--match S]`) lists live ids from a probe cached for a day; Claude has no list. Unsupported effort moves to the nearest supported value and the receipt records requested and applied values. `max` is retired: a request for it fails with an error suggesting `xhigh`; `ultra` is unchanged. Applied effort is only what reached the provider. A model the catalogue gives no effort control (no `efforts` list or `effort_transport: "none"`, for example Claude `claude-haiku-4-5`, agy `opus` and `sonnet`, Cursor `auto` and every OpenCode model) is sent no effort flag, even through an alias default. Its applied effort is empty, its route line has no `@effort` suffix, and the receipt keeps the requested value with the note `effort <x> ignored: <model> has no effort control`, shown once among the digest warnings. The run is not blocked. When no effort is sent, applied stays empty unless the provider reports one: Codex writes it to its session rollout, recorded with `effort_observed_source: codex:rollout.turn_context`. Fallback candidates and resumes re-resolve through the router, so they never resend an effort to such a model.

Writes require an owned registered worktree. Every `worktree_write` lane also may write the project's `.agent-run/locks/` (created if missing) without `add_dirs`, and no other `.agent-run/` entry outside its run directory. A writer may rebase or merge its integration branch over tracked `.agents/` files, but any writer's change to `.agents/` beyond its starting state and that branch is quarantined to `<attempt>/protected.patch` and removed from the lane, with a warning; the project policy's `instruction_changes` may allow or deny it instead. Land intended instruction edits by applying that patch deliberately. Only impossible execution or a hard boundary fails preflight: missing prompt, missing CLI, unowned writer worktree, credential-store exposure or write sandbox on a read-only run. Read-only guarantees vary by adapter and appear in the receipt; `best_effort` and `prompt_only` must not be claimed as enforced. On macOS, agy and OpenCode read-only launches use `sandbox-exec` when available to deny workspace reads outside `cwd` and `add_dirs`, and deny workspace writes. `applied.confinement` records `sandbox-exec` or `none`; without confinement, reads are unconfined. Agy reports `prompt_only` without OS confinement and `best_effort` with it. When a read-only run sets `cwd` different from `workspace_root` and the adapter has no OS read confinement, the plan warns `<adapter> read_only: cwd is not a read boundary`; this applies to Claude, Cursor, Kiro, and agy or OpenCode when `sandbox-exec` is unavailable. Codex's read-only sandbox reads everywhere and does not warn; it may write its attempt temp and `add_dirs` (such as `.agent-run/locks`), except an `add_dir` holding `cwd` or named with a pattern character (`*?[]{}`), which stays read-only with a warning. Every attempt points uv, npm, Corepack and XDG caches at `<attempt>/tmp/cache`; read-only attempts also disable pytest's cache provider and move bytecode, ruff and mypy caches there, so a reviewer can run a targeted test.

On macOS, Codex `read-only` and `workspace-write` runs and every `sandbox-exec` lane put the bundled `ps` shim, staged in the attempt's temp, first on the provider PATH. No sandbox can execute the setuid `/bin/ps`; the shim reads same-user process facts through libproc and supports the common `ps` fields and selectors. An inaccessible PID remains unverifiable, so liveness checks retain their conservative handling. Linux continues to use `/proc` and the normal `ps` command.

Take the returned run id and call `fabric_status` with `ids: [id]` and `wait_seconds: 55`. `fabric_output` gives bounded live tails. `fabric_cancel` stops the process group. A worker question yields `input_required`; reply with `fabric_dispatch` using `resume: id` and a new prompt. For one task of a batch, pass the run id plus `task_id`, or the task's own id. Terminal digests include the ready-to-paste provenance line and, when known, the session size (`ctx 212k/1M`). Resuming a large session re-reads its whole context. When the digest warns `! resuming a ~620k-token session`, prefer `fabric_dispatch` with `handoff: id` and a new prompt. It starts a fresh session primed with the prior route and result tail. A `session` name on `fabric_dispatch` starts a provider conversation the first time and resumes it after that, from any agent in the project; `fabric_session` inspects, lists or forgets the name. A provider without native continuation, or one that has lost the session, returns `continuation_unsupported`, and `fresh: true` hands the last clean result to a new session instead. `context_ceiling` (default 300,000 tokens) can lower Claude and Codex auto-compaction but never raises it. Where the provider already compacts earlier, or its point is unknown, the receipt records `provider_default`. Other adapters record `unsupported`. Direct CLI is a degraded path under [direct-cli-fallback.md](direct-cli-fallback.md).

Task class selects an adapter's `principal`, `senior` or `junior` model when no explicit model is chosen. The configured catalogue determines candidates; the receipt is authoritative for the applied route. A cooling explicit model still runs with a warning; alias routes skip cooling candidates. An account-level usage limit cools the whole adapter, except on agy, which meters each hosted model separately. Automatic fallback stays within permitted paid non-training routes unless the caller opts into `fallback: "any"` or an explicit list. A transient rate limit cools only the selected model and retries it once, including with fallback off. Backoff shares the cooldown account-and-host scope: 30 seconds initially, doubling to 15 minutes until success or an explicit clear. Provider reset and retry windows remain lower bounds. The cancellable wait adds up to 20% jitter and counts against the timeout, reserving at least 30 seconds for the retry. Configure `base_delay_seconds`, `cap_seconds`, `jitter_fraction` and `minimum_budget_seconds` under `rate_limit_pacing` in the existing routing catalogue or instance overlay. Daily quota exhaustion and OpenCode `FreeUsageLimitError` are usage limits and never trigger a same-model retry. Resumed sessions keep their no-retry policy. Writers retain their worktree lock and recheck their baseline after memory admission; an intervening change ends as `writer_changed`, without changing cooldowns or route health. Neither rate nor usage limits count as route failures. A lane whose provider produces no output within five minutes of launch ends as `startup_timeout`: treat it as a re-route signal, not a reason to retry the same route. It is retryable, so a dispatch that allows fallback moves to its next candidate automatically; with fallback off, dispatch again at once on another model or adapter. A writer falls back only when its worktree was clean and is unchanged; otherwise the receipt is not retryable and its fix says to inspect the worktree first.

## Route pools

Prefer a direct model name (`sonnet`, `sol`, `luna`, `deepseek-free`, `glm-free`, `flash`) when you know what the work needs; recorded dispatches use explicit names about three times as often as aliases, and a name is the most predictable selector. Reach for a pool when you want spread, rotation or a council.

The tier pools form a hierarchy ([tiered-council.md](tiered-council.md)): `utility` sits below junior for low-stakes, checkable classification, extraction, formatting, transforms and digests (Haiku at low effort first; free OpenCode members require explicit non-confidential public or synthetic input, with no overflow endpoint); `junior` is tier 1, laid-out work and cheap lenses that need little judgement (the zero-retention DeepSeek and GLM free models, Haiku and Luna lead at `high`, then agy Flash, with paid OpenCode Go at `sparing`; free models that train on prompts stay out of default pools); `senior` is tier 2, review, judgement and hard implementation (Sonnet and Sol, then agy Flash at high effort, with agy Opus 4.6 at `sparing`; Cursor is selectable by name only); `principal` is tier 3, critical decisions and consensus (Opus, then Astra, with Argon at `sparing`). `design` and `writing` serve those crafts. Utility defaults confidential and excludes free models even when they declare no training; `classify`, `extract`, `format`, `transform` and `digest` map to it. The three typed CLI verbs use plain code where possible, then utility, one retry and junior, with validation and per-call telemetry; `--non-confidential` permits free models only for public or synthetic input. Bulk utility dispatch has no automatic fallback in this slice. The optional Claude Bash output hook remains opt-in; harness registration and removal belong to state slice 4. The retired installer cannot register it. It uses the digest contract for output above 30 KiB and at most 200 KiB, makes one utility attempt without retry or junior escalation, and fails open within a 25-second deadline (30-second hook timeout). It emits only `updatedToolOutput` with the digest and private file/line pointers, and skips projects declaring protected paths because Bash cannot prove file provenance. Only selected remote `gh` reads and direct `cat`/`head`/`tail` file reads qualify; opaque commands keep their original output. A week of telemetry decides whether the hook earns its place. Nested worker calls, overflow and more verbs wait for telemetry.

`route: "utility" | "junior" | "senior" | "principal" | "design" | "writing"` picks from a weighted pool that spans adapters (`config/model-routing.json` `routes`; the user overlay may reorder or reweight). Weights are `high` (8), `normal` (4), `sparing` (1) or `off` (0), or any non-negative number on the same scale. An entry's `effort` band clamps the caller's effort in either direction, raising a low request or lowering a high one, and a warning names each move. Every mode skips a model that is not installed, is cooling down, whose adapter is disabled, or whose adapter cannot run it (the adapter-compatibility family and model gates), and names the skip in the warnings:

- default: the highest-weight available model, first listed on a tie;
- `rotate: true`: weighted round-robin per project (`route-rotation.json` under the Fabric state root);
- `council: N` (1–8): N runs as one batch (`<id>-1`…`<id>-N`), new families first, weights as odds (a prompt-training model joins only by its odds, never as the new family); when fewer than N models are available, members repeat and a warning says so;
- `models: ["adapter/model@effort", …]`: an ad-hoc council that bypasses routes. An unknown adapter prefix is rejected (`models_invalid`, naming the adapters); a model the catalogue does not register passes to its adapter as given, with a warning.

Mixed selectors settle by precedence rather than rejection, and a warning names every ignored field: `models` beats `route`, `alias`, `model`, `council` and `rotate`; an explicit `model` beats `route`, `council` and `rotate`; `route` beats `alias`; `council` beats `rotate`; an `alias` without a `route` ignores `council` and `rotate`. `adapter` with `route` narrows the pool to that adapter's entries.

`confidential: true` is a disclosure filter, not a selector: it works with a route, alias, model, adapter alone or task class, and removes every prompt-training model (`model_traits`, or a free model, that is a zero plan cap or `free_pattern` id, unless its own `trains_on_prompts` is `false`) from the resolved candidates, fallbacks included, even under `fallback: "any"`. An alias or task class whose first member is unsafe steps to its first safe member; dispatch refuses, with a fix, only when nothing safe remains. A resume or handoff of a confidential run stays confidential unless the caller passes `confidential: false`. Without it, such models stay in the pool; the warning appears only when one is actually selected. Task classes map onto routes through `route_synonyms` (`mid`, `second-opinion` and `implementation` → `senior`; `legwork`, `lens`, `mechanical`, `research` and `screenshots` → `junior`; `top`, `decision`, `critical-review` and `orchestration` → `principal`). The retired tier names `scout`, `bulk`, `workhorse`, `strong`, `review` and `flagship` are rejected with an error naming the replacement (`junior`, `senior` or `principal`), in a route, an alias or an overlay. The route line names the pick, for example `Route: opencode/deepseek-v4.1-flash (deepseek; observed; design council 2/3)`. `provenant routes` prints available models in full with live availability and
collapses each pool's unavailable or cooling models to a count line, with
`--all` for the full list; `--health` adds task-class routes and recent health (the same view as `provenant help routes`).

To add a model, list it in the user overlay (`~/.agents/config/model-routing.json`); unlisted product entries keep their place after the listed ones:

```json
{"routes": {"design": [
  {"model": "agy/gemini-3.8-flash", "weight": "high"},
  {"model": "opencode/opencode/mimo-v2.6-flash-free", "weight": "off"}
]}}
```

This puts Gemini first in `design` and switches one free model off. A spelling alias (`codex/gpt-6-sol` for `codex/gpt-6.1-sol`) addresses the same entry.

## Adapter alias tiers (relative, family-agnostic)

Within one adapter, `junior`, `senior` and `principal` are alias names (`alias: "senior"`): the adapter's own ranked models for a task class with no explicit model. One vocabulary serves both layers; the pools above span adapters, an alias stays within one.

| Tier | Use for | Reasoning effort |
|---|---|---|
| **junior** | bounded, objective work: extraction, classification, formatting, schema/grep checks, first-pass scouting | low |
| **senior** | research legwork, drafting, ordinary review, diff analysis, source mapping | medium |
| **principal** | sparingly: decomposition, final synthesis, resolving disagreements, hard/high-stakes calls | high |

Concrete alias candidates and their resolution order live only in
`config/model-routing.json`; verify them against runtime before execution.
The first configured candidate is the default and later candidates remain
admissible. `docs/model-dossier.md` records advisory preferences, so prose
alone does not move a default.

The live roster is `provenant routes`; pools and tiers live in
`config/model-routing.json`.

Task-class order, minimum effort and role policy also live in
`config/model-routing.json`. Resolution can advance to the next configured
route after a failed or empty outcome; it does not silently retry the same
model. Fabric retains up to 20 outcomes per adapter, model and task class for
seven days, and avoids a route whose latest outcome in that window failed or
returned empty output.

Each catalogue-configured risk tier has one bounded
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
`effort_capability_unverified`. The override is opt-in and is not the default
for crucial or terminal work: select the override only when a deliberately
different mind is wanted or the owner asks for it.

Worker models are never a silent principal fallback. Only the runtime capability
probe decides what the adapter can dispatch, and the catalogue defaults to
neither. Every substitution is recorded. Claude and Codex are equal primary
families.

Effort rule: **medium by default**; **high for verification, adversarial, and high-stakes** calls
(that's where subtle errors hide); reserve the very highest effort for isolated single-shot calls —
it can be slow and has been observed to hang inside agent loops, so don't run it in a tight loop.
Writers do not default to `xhigh`; use it only for a named escalation or a crucial review.

Cost is not just tokens: tools meter differently (tokens vs credits vs monthly caps), and the highest
tiers cost far more per call. A "small, objective" task only stays cheap if the schema is strict and
the output is validated — a loose schema lets a cheap model invent fields, which costs more in rework.

## Version-free family names

An adapter with `"latest_aliases": true` in `config/model-routing.json` (codex, claude, agy) resolves a
model id minus its version tokens (`gpt-sol`, `claude-sonnet`), or the last word alone (`sol`, `sonnet`)
when unambiguous, to the highest-versioned model of that family. It also considers the adapter's cached live
listing (codex has one): a newer id (`gpt-6.2-sol`) is picked with a note to run `provenant refresh-routing` or add it to
the catalogue, and effort, privacy and cooldown gates still apply (efforts come from the catalogue entry).
Explicit versioned ids stay exact. Claude has no live listing, so a new Claude model still needs one
catalogue line; the native Agent tool's `sonnet`/`opus` already track the latest.

## Cooldowns

After the user says usage is back or switches accounts, run `provenant route reset <adapter>`.

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
`opencode/deepseek-v4.1-flash-free` (zero-retention, no training) is the preferred go-to and
adapter default, `high` in `junior`; `opencode/glm-5.3-flash-free` (also zero-retention) is the
second. OpenCode has `senior` (`senior`) and `junior` (`junior`) aliases only: its models sit between the
Sonnet/Sol and Haiku/Luna tiers, so a `principal` request takes the adapter default and warns.
Paid `opencode-go/deepseek-v4.1-flash` is the `senior` backup; paid `opencode-go/glm-5.3-flash`
is reachable by name. The two paid models share one small monthly quota, so both are `sparing` in the pools. `opencode-go/gemini-4-argon-contributor` (`argon`) is a rare `sparing` entry in `principal`: it is a Contributor-tier model that trains on prompts and is not zero-retention, so never send it private or client data. Any registered or unregistered
model whose id matches the adapter's `free_pattern` (`-free$`) is free and, unless its
entry sets `trains_on_prompts: false`, may train on prompts: it is then warned on and
skipped by `confidential: true`. Free models
rotate, so adding or removing one is a single line in the adapter's `models` list
(or none, for a passed-through id); pools may weight it separately. Read-only
runs keep bash declared for Zen but deny every real command. Fabric honours
read-only `cwd` for OpenCode file access. On macOS, read-only OpenCode and agy
runs are confined by `sandbox-exec` to `cwd`, `add_dirs` and their own state:
home, shared temp and the rest of the workspace are unreadable. OpenCode may
also read the project config it loads between `cwd` and the repository root.
Every read-only profile reads the Python, uv and Node toolchain on PATH or in a
`.venv` between `cwd` and the repository root, including one installed under
home. A path is granted only when it resolves to a real toolchain install, and
then only its executable and runtime `lib`, `include` and `libexec` (plus a
framework build's library and `Resources`); a repository's `.venv` link or `pyvenv.cfg` alone grants nothing, and
home, its ancestors and credential stores are never granted. Use `opencode models`
to discover a live slug when overriding with `--model`.
Nested vendor ids attribute as that vendor; unparseable Zen free ids fall back
to `generic-open` (worker only, not assurance). See
[ADR 0025](../../../docs/adr/0025-broker-upstream-family-attribution.md).

A profile listing `codex` among its adapters reaches an OpenAI-compatible
endpoint instead, and must also declare `wire_api` (Codex 0.146 accepts only
`responses`). Codex receives the provider inline, as `-c model_providers.provenant_endpoint.*`
overrides plus `-c model_provider=provenant_endpoint`, rather than from the operator's own
`config.toml`; assurance runs also keep `--ignore-user-config`, while ordinary lanes inherit the
operator's config (ADR 0023, amendment). The token stays named: `env_key` tells Codex which variable to
read, so no credential reaches an argument vector. See
[ADR 0023](../../../docs/adr/0023-codex-custom-providers-inline-config.md).

## Default stack & fallback chains

Default once this skill triggers: **native same-harness workers ([native first](#native-first)) + at least one different-family
verifier/adversary** when a safe, data-authorised route exists. For high-stakes or low-oracle work, use
two different-family passes where practical. Prefer the safest adapter that can inspect the needed
artifact. If no safe external adapter is available under the host data policy, use objective local
checks and record `CROSS-FAMILY-NOT-RUN` instead of pretending it happened.

Bind each native stage to its task class. Route scan/extract stages to the
`junior` tier and reserve `principal` for synthesis/adjudication.

## Adaptive review topology

Review pressure follows the [`HARNESS.md` Review ladder](../../../HARNESS.md#review-ladder).
Routine work uses the chair and objective checks. Substantial work adds one
passing independent cross-family review when available. Crucial work needs at least
two passing reviews with distinct lens sets and two non-chair families when
available. Terminal work adds an adversarial lens to crucial coverage.

Every passing review needs a non-blank lens and a distinct reviewer. For each
missing non-chair family, record an unavailable or omitted leg with a non-empty
reason and a passing same-family replacement. Count each blocked family once,
excluding the chair's family and families already passing. A failed leg never
supplies coverage or a blocker record; receipt roles do not determine coverage.

The chair schedules reviews under a per-run configurable concurrency ceiling
and may sequence them around deterministic checks. It need not wait for a
particular model. Overlap creates defect pressure, not votes; objective checks
and source evidence remain authoritative.

Express chains by **role → tier/family**, resolving names at runtime:

```
verify      → safest different-family read-only adapter → objective checks → user if needed
adversary   → strong different-family critic → source/test-backed fix list
long-ctx    → long-context junior → file-backed synthesis → principal decision
bulk        → cheap diverse tier-1 lenses → strict schema → sampled verification
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
