# Execution: routing, providers and quota

Status: proposed for chair acceptance under [PORTING v7](../PORTING.md).
Author route: `codex/gpt-6.1-sol@high` via Fabric.
Command proposals feed the freeze in `design/cli.md`.

`routing` turns a task's model choice into an explainable selection; `providers`
builds the provider CLI invocation and interprets its output; `quota` supplies
current allowance, reset schedules and cooldown advice. `execution-run.md`
consumes the resolved route and launch specification and owns admission,
attempts, retries and publication. These crates never own provider lifetime,
writer leases or disclosure authority. The rewrite starts fresh; old JSON
shapes, state layouts and Python emulation are not contracts.

## Evidence and feature decisions

Source/count baseline: `1b3d48e4c549811ce3636edc30b047f36fd24ada`.
The review covers the requested Python routing, classifier, quota, utility,
provider/adapters/capabilities/home sources and tests, routing config, Fabric
callers and all three existing Rust crates. Reuse useful algorithms and wire
knowledge; discard `pending`, Python coercion/formatting and differential tests.

The local usage audit parsed Claude Bash and Codex exec-command tool-call fields
in 376 keyword-matched transcripts under `~/.claude/projects` and
`~/.codex/sessions`. Edits, quoted source, searches and test construction do not
count. On 22 October 2026, commands run `provenant routes` and resolve Agy/Claude
principal tiers, Cursor models and nested OpenCode IDs (Codex rollout
`01a162a9-09dc-7b33-a80a-d57571bd811f`). Of 33 direct dispatch invocations, 31
supply model and effort; five model values are shell placeholders, not known
resolved IDs. Actual task classes include mechanical and critical-review.
Claude transcript `1c21e420-fb1c-4a3f-be95-eb42e8489edb` runs quota/show JSON on
23 October. This sample excludes non-shell MCP activity and the other Mac;
absence below means no direct invocation found, not proof of global non-use.
No credentials were inspected or live model calls made for this design.

| Keep | Evidence and scope |
| --- | --- |
| Model/effort and native tier selection | Actual dispatch/resolve commands; orchestrate SKILL and `test_actual_effort.py`. Preserve explicit names, per-model controls and native subagent hand-offs. |
| Pools, rotation, councils | `runtime/fabric/src/pools.ts` is called by dispatch/batch; orchestrate documents them. No direct shell use found. Keep one implementation with a 1–8 member bound. |
| Preferences | A real `model_route.py select --task-class review` command and seeded `model-preferences.json`. Fold affinity/deprioritisation into the resolver, without a second selection engine. |
| Explicit class and size policy | Dispatch forwards task class; config maps class/size to tier/model/effort. Role stays descriptive. |
| Discovery | Preflight, provider_exec and Fabric call listings/capability checks. Keep cached per-model evidence and explicit verification. |
| Claude, Codex, Cursor, Agy, Kiro v3, OpenCode v2 | Implemented dispatcher arms and provider surface; fixtures below. Transcript route lookups prove selection use, not all provider launches. Activation requires the actual wire fixtures and confinement approval. |
| Endpoint profiles | Configured Claude/Codex integration and argv/env builders; no direct endpoint invocation found. Keep explicit profiles, disabled until fixture-backed, with separate endpoint/family identity. |
| Quota, account pins and cooldowns | Actual quota calls; dispatch consumes account/host-scoped readings and refusals. Keep automatic Claude account choice and explicit pins. |
| Current report/tick | Swift quota-menubar sources call tick and watch report.json. It is a concrete caller; no app launch was observed. Keep its current-state display and update that owned consumer at cut-over. |
| Safe provider home/config policy | Agy's mutable agentapi shim and Codex inherited hooks/MCP require the existing protections, implemented through confinement. |

| Drop | Evidence and replacement |
| --- | --- |
| Inferred task classification, shadow stages/cache and routing-context hook | Normal `cf_dispatch.resolve_routing` supplies explicit class, not brief/context. Classifier environment knobs have no external producer; model inference calls retired `extract`. The optional hook repeats skill advice; no invocation found. Keep explicit class/size mapping, no hidden model call. |
| `extract` and Ajv | Chair's decided retirement. Delete schema helper and extract callers. Digest/string-label classify belong to run/workflow's utility decision. |
| Copilot/Pi/Herdr execution | Compatibility config disables Copilot (no verified bounded invocation), disables Pi (no available trusted model), and calls Herdr visibility-only. No invocation found. Delete arms/rows and enable backdoors. |
| Separate `select`, command synonyms, typo guessing and old-tier migration | Multiple overlapping route engines and fresh state make these unnecessary. Used preference selection moves to resolve; invalid choices get a typed error. |
| Random council filling/repeated members | Owned implementation detail. Deterministic distinct members make decisions reproducible; insufficient eligible members yields a refusal. |
| Codex old-rollout quota/identity reads | R1 forbids old transcript/telemetry reads. Use current protocol evidence; absent observed model/effort remains unknown. |
| OpenCode Go spend estimator | Assumed undocumented $32 cap, one-machine message database and estimated resets. Delete those reads; allowance is unknown except for new provider refusals. |
| Manual credit ledger, signed-out history, fetch trace/history and forecasts | No credit/forget/trace/schedule invocation found; not required for routing. Display current provider credits; remove mutation, history forecasting and quota-to-telemetry storage. |
| General home mirroring/Python process shim | Keep only an allowlisted Agy home recipe. Confinement/run owns safe copy-back; supervisor owns process inspection and macOS ps support. |

## Selection rules

One `Selector` enum represents model, adapter-local tier, global pool, ad-hoc
model list or task class. CLI/Fabric normalise once: explicit `--model` wins
an accompanying tier hint with a note; incompatible pool/council selectors are
errors. Infer adapter only from a unique registered owner or explicit adapter
prefix; retain the rest of a broker's provider/model path. Ambiguous short names
require qualification. Exact IDs never silently upgrade. Unknown explicit IDs
pass unchanged on ordinary routes, with unverified identity. Native aliases and
`auto` remain provider choices until observed.

Resolve against one immutable catalogue/preferences/capability/quota snapshot:

1. Validate activation, selector and requested access. Expand configured
   adapter-local principal/senior/junior rosters, class mapping or pool.
   Declared size can raise a class default; concrete model selection stays
   concrete. Missing adapter tiers are unavailable, not arbitrary defaults.
2. Filter by family/model constraints, disclosure, executable and confinement
   capability, account/host, cooldown and current quota advice. Unknown quota
   is not exhausted; stale data cannot invent a refusal. Confidential selection
   excludes training/public-only/unknown-retention members and every unsafe
   fallback. Explicit names cannot bypass disclosure or confinement.
3. Rank by affinity, deprioritisation, weight, then catalogue order. Top takes
   the first. Rotate uses smooth weighted round-robin: high/normal/sparing/off
   are integer 8/4/1/0; custom weights are bounded non-negative integers.
   Councils choose distinct models, preferring distinct known families before
   repeating a family. Unknown/broker identity cannot prove assurance diversity.
   Automatic selection uses credit-only accounts last.
4. Resolve per-model effort and class/pool bounds. Record requested, sent and
   observed separately. Clamp with a reason and respect the class minimum;
   refuse an unsatisfiable floor. No-effort models send no control. Ordinary
   unknown IDs may receive an explicit unverified effort on adapters with that
   transport. OpenCode variant strings normalise once; explicit `--effort`
   wins a conflicting `#variant`. No invented default effort.
5. Return decisions, exclusion reasons, native hand-offs and ordered fallback
   requests. Run re-resolves each fallback within its retry budget. Explicit
   models default to no model fallback; explicit fallback lists remain filtered.

Fabric/run supplies `NativeSeat`. Implicit pools avoid externally launching
the host's own adapter and return native suggestions. Councils return native
and external members separately. Claude native tier models are haiku, sonnet,
opus for junior, senior, principal; Codex uses its native subagent model choices.
These are model choices, not effort. An explicit adapter/model may request
external execution under the host's policy; routing never spawns native agents.

Receipts distinguish selector, launch ID and observed answering ID, with family
provenance. A mismatch is evidence for run to reject/quarantine, never a signal
request. **Never enable fast or priority service.** Codex sends
`service_tier="default"` and removes conflicting inherited overrides. Other
profiles admit only recorded ordinary forms; reject fast/priority selectors.
A display name containing “Fast” does not authorise a fast service tier.

## Command proposal

For the command-tree freeze in `design/cli.md`. Every verb accepts `--json`:
one bounded `{version:1,status,data,warnings}` object plus newline. Errors use
`{version:1,status:"error",error:{code,message,fix?}}`; redacted diagnostics go
to stderr. Human results use tables, route lines and reasons.

| Command | Flags | Result |
| --- | --- | --- |
| `route list` | `--adapter`, `--pool`, `--all` | Pools/tier models, efforts, activation, installed/cached health and cooldowns; JSON adds catalogue revision. No implicit probes. |
| `route resolve` | `--model NAME` or `--tier principal\|senior\|junior` or `--pool NAME` or `--task-class NAME`; optional `--adapter`, `--effort`, `--endpoint`, `--account`, `--files`, `--lines`, `--access read-only\|worktree-write`, `--confidential`, `--native claude\|codex`, `--mode top\|rotate`, `--council N`, `--commit-selection` | Route/substitutions or `SelectionDecision`. Preview is pure; explicit commit advances rotation. Dispatch commits once at admission. |
| `route probe` | Required `--adapter`; `--refresh`; Claude-only `--verify --model --effort` | Capability source/time/warnings or `CapabilitySnapshot`. Listings by default; Claude canary requires explicit live-run authority. Refresh does not imply inference. |
| `route cooldown list` | `--adapter`, `--account` | Current-host scoped refusals/reset times or rows. |
| `route cooldown clear` | Required `--adapter`; optional `--model`, `--account` | Exact removed keys. Selected account/current host only, no global clear. Exhausted quota can still advise against selection. |
| `quota show` | `--provider`, `--account` | Cached windows/reset/source/age/credits or `QuotaReport`. No implicit refresh. |
| `quota tick` | `--trigger timer\|panel\|manual\|wake\|power`, `--power ac\|battery\|low`, `--provider`, `--account` | Due/refreshed/skipped rows, next due and report. Power only reschedules. |
| `quota path` | None | New report path or `{report_path}`, for the Swift caller. |

Exit 0 means completed; 1 means unavailable/refused/failed probe or busy;
2 means invalid input; 3 means I/O/publication failure; 130 means cancelled.
Tick publishes partial stale/unknown results and exits 0 unless every due fetch
fails (1). Unsupported listing is `unsupported_probe`, not an empty catalogue.

No new MCP tool is owned here. Fabric adapter/status views and dispatch/batch
use the same typed APIs; Fabric owns transport, native-seat detection and
cancellation. Ad-hoc model/fallback arrays belong in dispatch's typed input,
not another shell JSON interface. Build updates skills/front-door help to the
frozen names while preserving external SKILL.md frontmatter.

## Records, state and writes

`S: StateRoot` comes from `design/state.md`, a fresh root. Never derive another
root from old AGENT_FABRIC variables or read/migrate old .agents, .agent-run,
telemetry or transcript state. Proposed layout relative to that root:

| Record/file | Version 1 contents |
| --- | --- |
| Product `config/adapter-compatibility.json` | Enabled/reason, executable identity, ordinary wire profile, platform/access/family constraints and fixture requirements. Replaces YAML and SDK/Node metadata; providers owns it. |
| New config root `routing.json`, `routing-preferences.json` | Models/aliases, adapter tiers, pools, class/size policy, disclosure facts, endpoint SecretRefs; affinity/deprioritisation/weights. Config validates over product defaults; no old overlay reader. |
| `S/execution/capabilities/<adapter>/<scope-key>.json` | Executable identity/version, account/host, source, observed/expiry times, models/efforts and last refresh error. |
| `S/execution/selection/<project-id>.json` | Catalogue digest, pool scores and revision. Reset scores on membership/weight changes. |
| `S/execution/quota/<host>/<account-key>/<provider>.json` | Observation/source, windows/pools, balances, fetch/backoff and last good reading. |
| `S/execution/cooldowns/<host>/<account-key>.json` | Revision; adapter+canonical model or adapter-wide scope, reason/source/expiry. Quota owns the single store, routing consumes it. |
| `S/execution/quota/report.json` | Generated time, refreshing IDs, current account rows, headline and next due; atomically published after each fetch and on completion. No history. |

Endpoint profiles are versioned records under `routing.json.endpoints`:
`EndpointProfile { id, base_url, wire_api: Anthropic|Responses,
allowed_adapters, declared_family, token_secret_ref }`. URL parsing rejects
userinfo, query credentials and non-HTTPS URLs. A secret reference names an
authorised env/keychain source; token values are never persisted. Unknown
profiles, incompatible adapters and missing secrets fail closed before launch.

Records defines shared serde types; crates implement policy. `ResolvedRoute`
binds request, catalogue/capability revisions, adapter/endpoint/account/host,
launch model, effort transport, family provenance, disclosure and fallback
constraints. Account IDs are opaque, never emails or raw paths.
`EffortSelection` distinguishes requested/sent/observed. `QuotaWindow` contains
scope/pool, optional duration/start/reset, measured use and authority/source.
`QuotaSnapshot`, `Cooldown`, `SelectionDecision` and `QuotaReport` carry closed
status enums. Unknown usage is `None`, not zero; unknown duration is display-only.

Owned config/state rejects unknown fields and versions. Provider envelopes allow
extra fields but validate required types, duplicates and bounds. Records owns
bounded decoding: integers stay i64/u64, durations are integer milliseconds,
percentages fixed-point basis points, money scaled integers with currency/scale.
External fractions normalise at documented precision with checked bounds;
never pass integer tokens through f64. Jiff handles instants/named-zone resets;
ambiguous/nonexistent local times produce typed errors.

Use supervisor's safe atomic replace, file/parent fsync and bounded locks;
0700 directories/0600 files. Per-account refresh, per-scope cooldown and
per-project selection commits compare revisions. The CLI/run composition applies
`StateUpdate { key, expected_revision, next_record }` through supervisor; neither
routing nor quota writes files. Lock, re-read, compare revision, replace and
fsync form one store operation. Contention/stale revision is a typed retry or
refusal, never a silent uncommitted selection. Dispatch binds a selection token
to one admission so retries do not advance rotation again. A refresh claim has
an opaque token and deadline; expired claims cannot leave report rows refreshing.
No lock spans a provider wait:
reserve work, release, run, then conditionally commit. A stale completion cannot
overwrite newer evidence. Keep last good data on refresh failure and label age;
write errors return typed failures, never cancellation. No legacy adoption.

Quota distinguishes available, credit-only, exhausted, stale, unknown,
auth-required and unsupported. Independent model pools do not exhaust each
other. A newer comparable headroom observation or reset expiry reconciles only
that account/host quota refusal; unknown preserves it. Agy/OpenCode refusals stay
model-scoped unless the response proves wider scope. Account pins override quota
advice with a warning, not auth/disclosure/confinement/activation. Unknown reset
uses a one-hour cooldown, not permanent exhaustion.

Tick TTLs (timer/panel seconds): Claude 300/120, Codex 900/300, Cursor 1800/600,
Agy 1200/900. Manual has a 30s floor and respects provider backoff. Battery
multiplies intervals by two; low power by four and skips Agy. Network/429 failures
back off 120–3600s, auth 3600s, missing CLI 86400s. Retry-After seconds/date is a
lower bound capped at 86400s. Run at most three moderate/cheap probes; Agy alone.
Each probe has a deadline and 1 MiB capture bound; tick has a 55s wall budget and
returns next due instead of sleeping. Drop activity-file scanning and forecasts.

## Crate APIs and dependencies

Proposed signatures; shared inputs/results live in records to avoid cycles.
All return typed thiserror failures. Policy receives clocks and data explicitly.

```rust
// routing
fn resolve(&RouteRequest, &RoutingInputs) -> Result<SelectionDecision, RouteError>;
fn selection_update(SelectionRevision, &SelectionDecision)
    -> Result<StateUpdate, RouteError>;
// providers
fn profile(&ProviderRegistry, AdapterId) -> Result<AdapterPolicy, ProviderError>;
fn build(&ProviderRegistry, &ResolvedRoute, &LaunchInput, &ConfinementGrant)
    -> Result<LaunchSpec, ProviderError>;
fn discovery(&ProviderRegistry, AdapterId, &DiscoveryInput) -> Result<ProbePlan, ProviderError>;
fn decode_capabilities(AdapterId, &ProbeCapture)
    -> Result<CapabilitySnapshot, ProviderError>;
fn observe(&mut StreamState, ProviderInput)
    -> Result<Vec<ProviderObservation>, ProviderError>;
fn finish(StreamState, ObservedExit) -> Result<ProviderOutcome, ProviderError>;
fn continuation(&ProviderOutcome) -> Option<ContinuationToken>;
// quota
fn probe(&AccountScope, &QuotaProbeInput) -> Result<QuotaProbePlan, QuotaError>;
fn decode(&QuotaProbePlan, &ProbeCapture) -> Result<QuotaSnapshot, QuotaError>;
fn assess(&QuotaInputs) -> Result<Availability, QuotaError>;
fn schedule(&ScheduleInput) -> Result<RefreshDecision, QuotaError>;
fn reconcile(&CooldownInputs) -> Result<CooldownUpdate, QuotaError>;
fn report(&ReportInput) -> Result<QuotaReport, QuotaError>;
```

`ProviderRegistry` is an immutable validated config snapshot containing
activation, executable and endpoint policy. Compiled adapter mechanics are
constants; policy is not static. `build`/`discovery` cannot bypass that snapshot.
`CooldownUpdate` and snapshot/claim updates carry the same `StateUpdate` intent;
CLI/run applies it through supervisor and returns the committed revision.

`LaunchSpec` is in-memory executable identity, OsString argv/cwd, env delta,
stdin mode, decoder and preparation/finalisation recipe. Persist only a redacted
`LaunchSummary`, never prompt, env values or secret stdin. Bound argv prompts
to 64 KiB; Cursor needs open non-TTY stdin, Claude/Codex prompt stdin.
`ContinuationToken` binds adapter/session/account/host/model and sent controls;
reject changed identity/incompatible grants and replay only sent controls.

Observation parsing uses documented adapter positions, not recursive tool-output
search. Return assistant text, session/model evidence, retry/refusal/reset,
terminal and malformed-protocol events. Structured errors and diagnostics
outrank claimed success; answer prose discussing quota is not an error.
Recovered transport retries clear transient evidence. Terminal answer and process
exit are separate facts. OpenCode's exit-after-output exception requires recorded
terminal evidence; Agy timeout text stays partial. Providers never signals or
publishes a run result.

| Needed from owner | API/obligation |
| --- | --- |
| core/records/config | Fresh roots, IDs, clock values, canonical digest/bounded decimal codecs, non-secret account/host selection, effective routing and endpoint SecretRefs. Config replaces shared provider_identity.py. |
| confinement | Validate disclosure/access/executable identity; return ConfinementGrant, frozen config-inheritance decision and safe home recipe. Authorised secret resolution occurs at CLI/run boundary. Browser/Postgres capabilities are confinement policy. |
| supervisor via CLI/run | Execute probe/launch effects with custody, cancellation, bounded capture and secret stdin/env; safe fs/locks for commits. No process creation or raw handles in these crates. |
| run | Supply native seat/project/access/assurance/account scope; admission and selection commit, probe effects, retries, continuation, home finalisation and publication. Consume ResolvedRoute/LaunchSpec directly and record request/sent/observed values. |
| telemetry | Consume run's normalised attempt usage/error events. Quota does not read telemetry or import it. |
| Fabric/CLI | Thin API views, council task expansion and rendering; CLI freezes flags/exits. |

Keep R2 edges. Quota/routing return probe/store effects; CLI/run applies them.
Confinement owns the following launch obligations: Agy's private bin excludes tokens; safe copy-back retains live state on conflict.
Codex freezes clean/inherited config per attempt, disables unconfined hooks/MCP/
plugins for read-only or fails closed, and resets ambient endpoint overrides.
Writers inherit only the grant's permissions. Supervisor replaces ps support.

## External contracts and recorded fixtures

Activation requires recordings of the actual CLI form, with CLI version,
platform, redacted command, exit, provenance and redaction manifest. Capture
real output only with live-run authority. Replace IDs consistently; retain no
credentials, emails, org IDs, prompts or auth-store bytes. Synthetic malformed
cases supplement recordings; Python results never define expectations. Each adapter's capture set must show
CLI acceptance of its exact launch argv/env, model selector, effort transport,
ordinary service settings and fresh/resume form, including unsupported controls.
Native-host recordings must show Claude's tier model spellings and Codex's
selected native models; generated argv assertions alone do not prove acceptance.

| Adapter | Required recordings |
| --- | --- |
| Claude | Reduced auth-status; safe-mode/bare acceptance; fresh/resume stream-json, top-level versus tool/subagent identity, success/error/result, recovered retry, allowed/rejected rate-limit notice, unknown effort; canary modelUsage and endpoint env acceptance. Canary identity does not prove effort. |
| Codex | debug models per-model efforts; mcp list JSON with dotted names/plugins/local/HTTP; fresh/resume exec JSON thread/item/turn and recovery/error; permission profile/default tier/endpoint acceptance. Observed identity only from current protocol, never home transcript scanning. Quota app-server initialize/initialized, account/rateLimits/read, notification/error/reset-credit forms. |
| Cursor | models, fresh/resume init/model/session/result; ask+sandbox/open stdin acceptance; suffix effort/denial/error. Quota usage-summary billing/no-plan/401/429 and sanitised local-session/JWT schema, without real tokens. |
| Agy | tab-separated models/effort suffixes; fresh/conversation streams; flat/nested SUCCESS/PARTIAL/TIMEOUT, denied actions despite success, observed model; /usage pools/reset and opaque account binding. Fake filesystem fixtures prove shim/token/copy-back races. |
| Kiro | list-models JSON; **v3** fresh/resume non-interactive stream-json, auto omission, sessionUpdate/runFinished/identity/denial/error. A v2 recording cannot authorise v3 or an enforcement claim. Negative write probes require separate authority and bind executable/expiry. |
| OpenCode | **v2** models/provider aliases; standalone run JSON fresh/resume, model#variant, permission/MCP env, step/tool/terminal, quota/variant/unavailable diagnostics, terminal exit-after-output; session export model identity. No v1/shared-server fallback. |

Claude quota also needs real sanitised OAuth usage windows/dollar/minor-unit,
missing/advisory windows, resets, 401 and Retry-After fixtures. Claude/Cursor
HTTPS uses system `curl --config -`, headers only on stdin, never argv/env/disk
or logs. Use fixed allowlisted HTTPS URLs, no redirects/verbose/trace, bounded
time/body and system trust/proxy behaviour per ADR 0028. Authorised boundary
resolution keeps secrets in ephemeral memory; redact responses before storing.

Existing recorded seeds: `tests/fixtures/model-listings/{cursor-models.txt,
opencode-models.txt,kiro-models.json}`, capture notes 23 October 2026.
`fabric-context/` contains stream candidates; test_provider_exec identifies live
Agy nested-result/Kiro v2 examples from 23 September. Audit/sanitise provenance
before promotion. Codex/Agy listings, current Kiro v3, full launch/resume and
quota sets are build acceptance work, not recordings already obtained.
`fabric-v1/provider-*.json` is synthetic. New fixtures live at
`tools/front-door/tests/fixtures/providers/<adapter>/` with capture manifests.

No hook remains owned here. CLI/config removes the optional routing hook's
SessionStart/PreToolUse registration with its own protocol fixtures. SKILL.md
frontmatter, MCP JSON-RPC and Git are external contracts handled by records,
Fabric and confinement; update command consumers without duplicating protocols.

## Tests and acceptance (R9)

Unit tests beside modules inject clocks/config/evidence. Cover selectors,
native tiers/hand-offs, exact/alias identity, effort transport/clamps/floors,
confidential fallback filtering, deterministic diversity/rotation revisions,
config/endpoint/secret failures and decode bounds; verify R2 layering. Cover nested tool identity, recovered retries,
terminal/exit combinations; quota unknown/zero/pools/account/host, stale and
reset reconciliation, DST, Retry-After, pins and backoff. Do not copy validators
made redundant by Rust types or keep Python oracles.

Binary integration tests use fake providers and isolated S: real CLI/MCP views,
JSON/exits, no process on help/list/show/resolve, explicit refresh/canary, no old
root access, failed cache keeping last good, and secrets absent from argv/files/
diagnostics. Replay recordings through the actual decoder; run consumes the same
route/launch spec. Update Swift report decoding/fixtures together, removing
historical forecast and old-CLI fallbacks.

Linux/macOS process fixtures use FIFO/handshakes, never sleeps: competing
selection/cooldown writers, late probe completion, cancellation isolated from
siblings/healthy providers, output overflow, ENOSPC, terminal-before-exit and
home races. Supervisor proves custody/signalling; run proves one writer and
terminal publication. Bookkeeping failures cannot generate stop requests or
lose publication. Compare canonicalised macOS temporary paths.

Local: fmt/clippy and touched-crate tests. CI: workspace integration/process and
release macOS checks plus remaining tests with subjects. No heavy local harness
run or unauthorised live smoke. Enable each adapter only after recorded wire and
confinement acceptance; chairs accept design after cross-family review.

## Deletion ledger

Counts are `wc -l` at the source baseline, including comments and blanks.
Whole-file Python/TS removals below are this area's completion obligation;
Fabric wrapper deletion lands with its Rust consumer, not an adapter bridge.
Mixed files are assigned separately so two areas do not claim the same saving.

| Python source removed | Lines |
| --- | ---: |
| `scripts/model_route.py` | 1,614 |
| `scripts/model_route_catalog.py` | 279 |
| `scripts/model_route_cli.py` | 471 |
| `scripts/model_route_live.py` | 404 |
| `scripts/model_route_pools.py` | 702 |
| `scripts/model_route_preferences.py` | 567 |
| `scripts/model_route_probe.py` | 404 |
| `scripts/task_classifier.py` | 190 |
| `scripts/routing_context_hook.py` | 85 |
| `skills/orchestrate/scripts/capabilities.py` | 374 |
| `skills/orchestrate/scripts/exec_routing.py` | 370 |
| `skills/orchestrate/scripts/provider_output.py` | 548 |
| `skills/orchestrate/scripts/adapters/__init__.py` | 17 |
| `skills/orchestrate/scripts/adapters/agy.py` | 31 |
| `skills/orchestrate/scripts/adapters/claude.py` | 65 |
| `skills/orchestrate/scripts/adapters/codex.py` | 313 |
| `skills/orchestrate/scripts/adapters/copilot.py` | 34 |
| `skills/orchestrate/scripts/adapters/cursor.py` | 22 |
| `skills/orchestrate/scripts/adapters/kiro.py` | 48 |
| `skills/orchestrate/scripts/adapters/opencode.py` | 112 |
| `scripts/provider_quota.py` | 1,611 |
| `scripts/quota_schedule.py` | 326 |
| `scripts/quota_display.py` | 399 |
| `scripts/quota_providers/agy.py` | 162 |
| `scripts/quota_providers/claude.py` | 193 |
| `scripts/quota_providers/codex.py` | 174 |
| `scripts/quota_providers/cursor.py` | 78 |
| `scripts/quota_providers/opencode.py` | 182 |

| Tests and Python helpers removed | Lines |
| --- | ---: |
| `tests/test_model_route.py` | 4,790 |
| `tests/test_model_route_pools.py` | 986 |
| `tests/test_task_classifier.py` | 107 |
| `tests/test_capabilities.py` | 404 |
| `tests/test_actual_effort.py` | 284 |
| `tests/test_provider_quota.py` | 2,188 |
| `tests/test_quota_schedule.py` | 195 |
| `tests/test_quota_display.py` | 580 |
| `tests/support/quota.py` | 54 |
| `apps/quota-menubar/Fixtures/make_fixtures.py` | 103 |
| `tools/front-door/crates/quota/src/tests/oracle.py` | 84 |

| TypeScript removed with Fabric integration | Lines |
| --- | ---: |
| `runtime/fabric/src/catalogue.ts` | 281 |
| `runtime/fabric/src/pools.ts` | 210 |
| `runtime/fabric/tests/catalogue.test.ts` | 151 |

Whole-file Python/TS total: **20,192 lines removed**. Also remove the shell
wrappers `scripts/model-route` (15) and `scripts/provenant-quota` (5),
`config/adapter-compatibility.yaml` (306; replaced by JSON), and seven synthetic
`tests/fixtures/fabric-v1/provider-*.json` files (65 each, 455 total).
Keep and extend real listing/stream recordings; they are not deletion savings.

Remove these old golden judges with their subject: quota in build step 2,
route/routes/capabilities/extract in step 3. Shared golden runner/manifest/tests
belong to CLI's final golden retirement; remove these registrations there.

| Golden fixture removed | Lines |
| --- | ---: |
| `tests/golden/cases/route/route-case-01.json` | 12 |
| `tests/golden/cases/route/route-case-02.json` | 12 |
| `tests/golden/cases/route/route-resolve.json` | 13 |
| `tests/golden/cases/routes/routes-case-01.json` | 12 |
| `tests/golden/cases/routes/routes-case-02.json` | 12 |
| `tests/golden/cases/routes/routes-catalog.json` | 12 |
| `tests/golden/cases/capabilities/capabilities-case-01.json` | 12 |
| `tests/golden/cases/capabilities/capabilities-case-02.json` | 12 |
| `tests/golden/cases/quota/quota-case-01.json` | 12 |
| `tests/golden/cases/quota/quota-case-02.json` | 12 |
| `tests/golden/cases/extract/extract-case-01.json` | 12 |
| `tests/golden/cases/extract/extract-case-02.json` | 14 |

| Shared file | Lines at baseline | Deletion boundary |
| --- | ---: | --- |
| `skills/orchestrate/scripts/provider_exec.py` | 2,760 | Run/supervisor deletes the whole file once its lifecycle lands; adapter builders/observations above replace only the provider portion. |
| `tests/test_provider_exec.py` | 9,272 | Run/supervisor removes this suite with that subject, after new custody/process tests exist. |
| `skills/orchestrate/scripts/agy_home.py` | 268 | Confinement/run deletes the file after the safe home recipe and supervisor shim replacement land. Providers supplies adapter policy, not syscalls. |
| `scripts/provider_identity.py` | 133 | Config deletes once quota/telemetry/run/Fabric consume its typed identity API. |
| `scripts/utility.py` | 241 | Remove extract/validation arms here. Run/workflow owns whole-file deletion or replacement for digest/string-label classify. |
| `tests/test_utility.py` | 154 | Remove extract cases with the arm; remaining utility subject follows run/workflow. |
| `scripts/utility-schema.cjs` | 16 | Remove entirely with extract; this is JavaScript, not TypeScript. |
| `runtime/fabric/src/execution-input.ts` | 610 | Fabric/run replaces the mixed file. Delete route selector helpers and `routeArguments` here; do not count the entire file twice. |

Delete adapter/classifier-specific assertions from shared `test_cf_dispatch`,
`test_dispatch_run`, `test_fabric_records`, installation and front-door tests
when their owners land; their surviving subjects are not whole-file removals.
Shared fake-CLI/module helpers survive until their final caller is removed.
The optional digest hook and its test stay with the utility/workflow decision.

The three existing crates contain **18,275 Rust lines** (routing 8,230,
providers 4,519, quota 5,526), including inline tests. Replace these owners,
retaining useful algorithms but discarding all old wire/format emulation.
Whole-file emulation deletions include routing `casefold.rs` (2,329),
`pending.rs` (424), `ordered_json.rs` (121), `oracle.rs` (116); providers
`pending.rs` (171), `ordered_json.rs` (278), `oracle.rs` (135),
`dispatch_contract.rs` (385), `exec_contract.rs` (192); quota `pending.rs`
(1,048), `pending/python_unicode.rs` (749), `oracle.rs` (114) and the obsolete
`policy_tests.rs` (1,553). Remove every inline differential/oracle test and
Python formatting helper in the rewritten modules as well. The quota Python
oracle's 84 lines are already in the Python table, not this Rust count.

Planning budget: 5,000–7,000 new Rust lines including adapter parsers, policy,
unit tests and binary integration tests. Against 20,192 Python/TS and
18,275 existing Rust lines, expected net code/test change is
**−33,467 to −31,467 lines**. This excludes mixed-owner deletions,
config, shell, JSON fixtures, documentation and new real recordings. It is an
estimate, not a measured delivery result; each build PR reports its actual
`Net lines:` across Python/TS/Rust and explains growth.

## Build order

1. **Provider records and profiles.** After core/records/config/confinement,
   land typed model/effort/launch/observation records, six adapter profiles and
   bounded stream parsers with the available real recordings. Delete Python
   adapters and `provider_output.py`, their synthetic status fixtures and
   providers' Rust emulation/contracts in the same PR. Disabled adapters are
   removed, not carried as pending implementations. Run callers await their
   owner; no compatibility bridge keeps the deleted Python API alive.
2. **Quota and refresh.** Land current quota parsers, schedule, scoped cooldown
   policy, new-root snapshot/report store effects and fake-curl/probe tests.
   Delete quota Python modules, their tests/support/generator and the quota
   Rust emulation/oracle suite. Land the proposed quota CLI and update the
   Swift consumer/fixtures to the new root/report together. Config supplies
   identity and authorised secret access before enabling HTTP probes.
3. **Routing.** Land one resolver, model/tier/class selection, pools,
   preferences, native-hand-off decisions, discovery plans and rotation
   commit. Replace compatibility YAML with the typed JSON registry. Delete
   `model_route*.py`, classifier/hook, `exec_routing.py`, `capabilities.py`,
   their whole-file tests and routing Rust emulation. Land the route CLI,
   update skills/help/config installation, remove extract/schema validation
   and remove obsolete hook registration through the config owner.
4. **Execution/Fabric composition.** With execution-run, consume route and
   launch types directly, implement probe effects and atomic state commits
   through supervisor, and delete TS catalogue/pool wrappers and their test.
   Remove mixed-file route serialisation and shared Python files once their
   Rust owner lands; bind each deletion to one owner in the PR. The
   run/supervisor acceptance proves cancellation, custody and publication;
   this lane proves selection and wire decoding. Finish missing authorised
   CLI recordings before enabling their profiles, then run the isolated-root
   fake-provider dispatch/MCP end-to-end gate.

Each step is a PR-sized responsibility slice; adapter fixture/profile work
may split by adapter with its corresponding Python module deleted in that PR.
All land Rust and remove the subject/tests they replace together. Python callers
whose own owner is later are allowed to break under R10; do not add selectors,
transition readers or inert copies. Build lanes open PRs; chairs arrange the
cross-family council and accept/merge on CI. This design PR changes only this
file and does not arm auto-merge.
