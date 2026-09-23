# Fabric v2 contract

Status: design record, 2026-09-23. Source: the reviewed Fabric v2 design in the project research run. The integration gate in that design controls rollout; this document records the durable interface and operating decisions.

## Decisions

Fabric is the default front door for external provider work. A chair uses native subagents for its own models and Fabric for other providers, long runs and worktree writers. Direct CLI remains a named degraded path. Fabric must keep the happy path terse, warn when a runnable route needs substitution, and give a typed terminal state instead of hanging quietly. One supervisor owns each attempt and its provider process group. Full output stays in run files.

The default MCP surface has twelve tools: `fabric_dispatch`, `fabric_status`, `fabric_cancel`, `fabric_output`, `fabric_adapters`, `fabric_whoami`, `fabric_send`, `fabric_inbox`, `fabric_acknowledge`, `fabric_note`, `fabric_activity`, and `fabric_task`. These are the tools registered by `runtime/fabric/src/server.ts`; `FABRIC_LEGACY_TOOLS=1` also registers `fabric_batch`, `fabric_team_create` and the earlier task tools.

| Tool | Registered request |
|---|---|
| `fabric_dispatch` | One top-level `prompt` or `prompt_file`, `tasks[]` (1–64, `concurrency` 1–8), `resume` with a new prompt, or `handoff` with a new prompt. Optional route and control fields include `adapter`, `alias`, `model`, `effort`, `mode`, `worktree`, `cwd`, `network`, `sandbox`, `add_dirs`, `fallback`, `context_ceiling`, `task_id`, `timeout_seconds`, `wait_seconds` (0–55), and `detail`. With `resume` or `handoff`, `task_id` selects one task of a batch. |
| `fabric_status` | `ids[]` of run, task or batch IDs, `wait_seconds` (0–55), `until: any|all`, and `detail`; `id` is also accepted for one run. One row per run. |
| `fabric_cancel` | Required `id`, optional `reason`; stops the owner and provider group. |
| `fabric_output` | Required `id`, optional `part: result|stderr|events|receipt`, `offset`, and `max_bytes` (1–20,000); returns a bounded chunk and `next_offset`. |

A worker question yields `input_required`; `fabric_dispatch` with `resume` appends an attempt to the same run. `resume` takes a run ID, a run ID plus `task_id` for one task of a batch, or the task's own ID. A `task_id` with no prior attempt is rejected as `resume_task_unknown`. A resume may change only `context_ceiling`; any other route or control change needs a new dispatch. For a batch, use `fabric_dispatch` with `tasks[]`; wait on returned IDs with `fabric_status`.

A run has `queued`, `running` and attempt-terminal states. Terminal statuses are `ok`, `partial`, `failed`, `usage_limited`, `rate_limited`, `auth_required`, `model_unavailable`, `permission_blocked`, `stalled`, `timed_out`, `cancelled`, `interrupted`, `rejected`, `tool_missing`, and `input_required`. Structured provider events take precedence over text signatures. Fallback creates another attempt under the same run id. Alias routes default to fallback through allowed paid non-training routes; an explicit model defaults to no fallback. Free or prompt-training routes require explicit opt-in.

## Session context

Many routes have 1M-token windows, so resuming a large session can cost far more than starting fresh. Fabric measures each attempt's context, caps it where the provider allows, and warns rather than blocks.

Each attempt records `context: {context_tokens, input_tokens, output_tokens, cached_input_tokens, context_window_tokens, context_percent, source}`. `context_tokens` is the session's current size after the turn. `input_tokens` counts all prompt tokens, cached ones included; `cached_input_tokens` is the cached share. `source` is `observed` when the provider reported its latest request, `estimated` when only turn totals exist (these overstate context when a turn made several requests), and `null` when nothing was reported. Fields the provider did not report stay `null`; Fabric never invents them. The field paths come from one live turn per adapter, retained as `tests/fixtures/fabric-context/`:

| Adapter | Context source | Window | Ceiling control |
|---|---|---|---|
| claude | last request usage (`result.usage.iterations`), `observed` | the answering model's `modelUsage` `contextWindow` | `--autocompact <n>` below its compaction point |
| codex | rollout `token_count.last_token_usage`, `observed`; the stream's `turn.completed` totals alone are `estimated` | rollout `model_context_window` | `-c model_auto_compact_token_limit=<n>` below its compaction point |
| cursor | `result.usage` turn totals, `estimated` | not reported | none, `unsupported` |
| opencode | last `step_finish` `tokens.total`, `observed` | not reported | none, `unsupported` |
| agy | last `step_update` usage, `observed` | not reported | none, `unsupported` |
| kiro | `metadata.contextUsagePercentage` only, so `context_percent` | not reported | none, `unsupported` |
| copilot | nothing reported, all `null` | — | none, `unsupported` |

`context_ceiling` is an optional token count. Its default comes from `context.ceiling_tokens` in `config/model-routing.json` (300,000). A value outside 100,000–1,000,000 is clamped with a warning, never rejected. The ceiling only lowers a provider's compaction point; it never raises it. Codex compacts at `effective_context_window_percent` of the model's `context_window` in `$CODEX_HOME/models_cache.json` (about 258k for a 272k model). Fabric never passes `model_context_window`. Claude compacts at the model window: 1M for `opus`, `sonnet` and `fable`, 200k for `haiku`. A read-only `haiku` route may be answered by a 1M model (see provenance), but the ceiling is still bounded by haiku's 200k. If the user's `autoCompactWindow` in `~/.claude/settings.json` is lower, Claude compacts there instead: dispatched `--safe-mode` runs load user settings. The flag is passed only when the ceiling is below that point. When the point is unknown (a model missing from the Codex cache, no cache, or an unlisted Claude model), no flag is passed, because lowering cannot be proven. The attempt records `provider_default` with a `null` point and warns once.

The attempt records `applied.context_ceiling` as `enforced`, `provider_default` (the provider's own point is at or below the ceiling, with `applied.context_ceiling_source`) or `unsupported`. It also records `applied.context_ceiling_tokens`, the point in force (`null` when unknown), and `applied.context_ceiling_requested`. A resume inherits the prior requested ceiling unless the call passes a new one.

A resume reads the prior attempt's context. It adds a digest warning when that context exceeds the effective ceiling (the point in force, else the requested ceiling), or when the size is unknown and the adapter has no ceiling control, for example `! resuming a ~620k-token session; fresh: fabric_dispatch{prompt, handoff:"<run id>"}`. The resume still runs. `handoff: <run id>` (with `task_id` for a batch task) is the cheaper alternative. It starts a fresh run whose prompt is prefixed with the prior task's route line and its result tail, at most 8,000 bytes in total. If the call names no adapter, alias or model, the handoff reuses the prior adapter, model and effort, and a prior writer's mode and worktree. The prior task must be terminal.

A terminal digest appends a compact marker to its Route line: `ctx 212k/1M` when observed, `ctx ~19k` when estimated, `ctx 8%` from a percentage, and nothing when unknown. The stored provenance line stays unmarked for trailers and the index. Brief status rows omit `context`; `detail: "full"` includes it.

## Receipt and provenance

An owner writes `fabric.attempt.v1` at `tasks/<id>/attempt-NNN/attempt.json` and aggregates it into `RUN_RECEIPT.json`. A status row is `fabric.status.v1`: the latest attempt plus attempt history, batch id where relevant, and live worktree ledger fields. The attempt records state, status, mode, cwd, worktree, timing, process group, session id, retry fields, evidence, question, applied sandbox/network/additional directories/guarantee/context ceiling, context, warnings, provenance, paths and digest. Unknown fields may be ignored; removals bump the version.

Provenance records requested route, resolved and observed model, observation source, identity (`observed`, `resolved`, `unknown`), provider, transport, family, requested and applied effort, CLI version, fallback origin, notes and the generated line. A terminal digest and status row carry `Route: adapter/model@effort (provider; identity)`; the retained index stores that line after run pruning. The proposed `provenant route <run-id>` index lookup is not yet wired: the current command invokes the model catalogue router. Until it is, copy the line from the digest or status row and use `Agent-Route: adapter/model@effort` for the commit trailer. Never infer a model from a worker self-report. Claude's observed model is the one on the assistant messages that answered (`claude:assistant.message.model`), else a lone `result.modelUsage` key, and `system.init.model` only when neither exists. In `--permission-mode plan`, used for every read-only Claude run, Claude Code 2.1.280 reports `haiku` in init but `claude-sonnet-5` (1M) answers; Opus stays Opus, and a writer's haiku answers as haiku (200k). The attempt records `init_model` and `answered_models` and warns `claude answered as <x>; init reported <y>`. If several models answered, the final top-level one goes on the Route line. Native Claude subagents use the Agent tool model parameter and record `claude/<model>@<effort> (anthropic; resolved)`.

## Layout and retention

Run root is the primary checkout root, including when dispatch starts inside a linked worktree. Runs live at `.agent-run/runs/<YYYYMMDD-HHMM>-<kind>-<slug>-<rand6>/`; owner logs live in `_owner/`. `.agent-run/sessions/` holds chair checkpoints and `.agent-run/scratch/` holds disposable files. `.worktrees/` names derive from branches with `/` replaced by `-`. New repositories exclude `.agent-run/`, `.worktrees/` and legacy `.work/` from Git locally.

`provenant clean` plans by default and prints a digest. `--apply --plan sha256:<digest>` reevaluates before deletion; `--human-authorised` records the caller's attestation of human authority for worktree removal. It does not itself prove who approved removal. Live, pinned, cited, unaccepted delivery, resumable and active mission runs are protected. Successful and cancelled runs retain seven days; typed failures retain fourteen; accepted deliveries retain thirty; owner logs retain seven; scratch retains one day. Sessions are durable handoffs and remain triage-only. Unknown paths are triage-only. If GitHub PR state cannot be read, the plan warns and holds run deletion; Git-proven merged worktrees can still expire. Worktrees require clean state and merge proof, then removal through `scripts/worktree remove`. The append-only run index keeps provenance for at least 365 days. Legacy `mcp-*`, timestamped orchestration, delivery `RUN.json` and mission `GOAL.md` readers remain for one release.

Project `CLAUDE.md` keeps both `@AGENTS.md` and `@HARNESS.md`. `scripts/install-harness` writes only a bootstrap pointer to the instance `AGENTS.md` and product `HARNESS.md` in `~/.claude/CLAUDE.md`; it does not copy their contents. Both project imports therefore provide the local rules and constitution without relying on a global file containing them.

## Routing and authority

`model_route.py snapshot --json` is the single merged catalogue source. Unknown model IDs pass through with a note when runnable; unsupported effort substitutes to the nearest supported value. Explicit cooling models run with a warning. A hard rejection is reserved for impossible execution or a hard boundary. Per-run flags are preferred; editing global provider configuration requires explicit authority. Credentials never appear in argv, receipts or logs. Provider guarantees are reported as `enforced`, `best_effort` or `prompt_only` according to observed controls.
