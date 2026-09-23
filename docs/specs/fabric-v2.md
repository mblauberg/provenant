# Fabric v2 contract

Status: design record, 2026-09-23. Source: the reviewed Fabric v2 design in the project research run. The integration gate in that design controls rollout; this document records the durable interface and operating decisions.

## Decisions

Fabric is the default front door for external provider work. A chair uses native subagents for its own models and Fabric for other providers, long runs and worktree writers. Direct CLI remains a named degraded path. Fabric must keep the happy path terse, warn when a runnable route needs substitution, and give a typed terminal state instead of hanging quietly. One supervisor owns each attempt and its provider process group. Full output stays in run files.

The default MCP surface has twelve tools: `fabric_dispatch`, `fabric_status`, `fabric_cancel`, `fabric_output`, `fabric_adapters`, `fabric_whoami`, `fabric_send`, `fabric_inbox`, `fabric_acknowledge`, `fabric_note`, `fabric_activity`, and `fabric_task`. These are the tools registered by `runtime/fabric/src/server.ts`; `FABRIC_LEGACY_TOOLS=1` also registers `fabric_batch`, `fabric_team_create` and the earlier task tools.

| Tool | Registered request |
|---|---|
| `fabric_dispatch` | One top-level `prompt` or `prompt_file`, `tasks[]` (1–64, `concurrency` 1–8), or `resume` with a new prompt. Optional route and control fields include `adapter`, `alias`, `model`, `effort`, `mode`, `worktree`, `cwd`, `network`, `sandbox`, `add_dirs`, `fallback`, `task_id`, `timeout_seconds`, `wait_seconds` (0–55), and `detail`. |
| `fabric_status` | `ids[]` of run, task or batch IDs, `wait_seconds` (0–55), `until: any|all`, and `detail`; `id` is also accepted for one run. One row per run. |
| `fabric_cancel` | Required `id`, optional `reason`; stops the owner and provider group. |
| `fabric_output` | Required `id`, optional `part: result|stderr|events|receipt`, `offset`, and `max_bytes` (1–20,000); returns a bounded chunk and `next_offset`. |

A worker question yields `input_required`; `fabric_dispatch` with `resume` appends an attempt to the same run. For a batch, use `fabric_dispatch` with `tasks[]`; wait on returned IDs with `fabric_status`.

A run has `queued`, `running` and attempt-terminal states. Terminal statuses are `ok`, `partial`, `failed`, `usage_limited`, `rate_limited`, `auth_required`, `model_unavailable`, `permission_blocked`, `stalled`, `timed_out`, `cancelled`, `interrupted`, `rejected`, `tool_missing`, and `input_required`. Structured provider events take precedence over text signatures. Fallback creates another attempt under the same run id. Alias routes default to fallback through allowed paid non-training routes; an explicit model defaults to no fallback. Free or prompt-training routes require explicit opt-in.

## Receipt and provenance

An owner writes `fabric.attempt.v1` at `tasks/<id>/attempt-NNN/attempt.json` and aggregates it into `RUN_RECEIPT.json`. A status row is `fabric.status.v1`: the latest attempt plus attempt history, batch id where relevant, and live worktree ledger fields. The attempt records state, status, mode, cwd, worktree, timing, process group, session id, retry fields, evidence, question, applied sandbox/network/additional directories/guarantee, warnings, provenance, paths and digest. Unknown fields may be ignored; removals bump the version.

Provenance records requested route, resolved and observed model, observation source, identity (`observed`, `resolved`, `unknown`), provider, transport, family, requested and applied effort, CLI version, fallback origin, notes and the generated line. A terminal digest and status row carry `Route: adapter/model@effort (provider; identity)`; the retained index stores that line after run pruning. The proposed `provenant route <run-id>` index lookup is not yet wired: the current command invokes the model catalogue router. Until it is, copy the line from the digest or status row and use `Agent-Route: adapter/model@effort` for the commit trailer. Never infer a model from a worker self-report. Native Claude subagents use the Agent tool model parameter and record `claude/<model>@<effort> (anthropic; resolved)`.

## Layout and retention

Run root is the primary checkout root, including when dispatch starts inside a linked worktree. Runs live at `.agent-run/runs/<YYYYMMDD-HHMM>-<kind>-<slug>-<rand6>/`; owner logs live in `_owner/`. `.agent-run/sessions/` holds chair checkpoints and `.agent-run/scratch/` holds disposable files. `.worktrees/` names derive from branches with `/` replaced by `-`. New repositories exclude `.agent-run/`, `.worktrees/` and legacy `.work/` from Git locally.

`provenant clean` plans by default and prints a digest. `--apply --plan sha256:<digest>` reevaluates before deletion; `--human-authorised` records the caller's attestation of human authority for worktree removal. It does not itself prove who approved removal. Live, pinned, cited, unaccepted delivery, resumable and active mission runs are protected. Successful and cancelled runs retain seven days; typed failures retain fourteen; accepted deliveries retain thirty; owner logs retain seven; scratch retains one day. Sessions are durable handoffs and remain triage-only. Unknown paths are triage-only. If GitHub PR state cannot be read, the plan warns and holds run deletion; Git-proven merged worktrees can still expire. Worktrees require clean state and merge proof, then removal through `scripts/worktree remove`. The append-only run index keeps provenance for at least 365 days. Legacy `mcp-*`, timestamped orchestration, delivery `RUN.json` and mission `GOAL.md` readers remain for one release.

Project `CLAUDE.md` keeps both `@AGENTS.md` and `@HARNESS.md`. `scripts/install-harness` writes only a bootstrap pointer to the instance `AGENTS.md` and product `HARNESS.md` in `~/.claude/CLAUDE.md`; it does not copy their contents. Both project imports therefore provide the local rules and constitution without relying on a global file containing them.

## Routing and authority

`model_route.py snapshot --json` is the single merged catalogue source. Unknown model IDs pass through with a note when runnable; unsupported effort substitutes to the nearest supported value. Explicit cooling models run with a warning. A hard rejection is reserved for impossible execution or a hard boundary. Per-run flags are preferred; editing global provider configuration requires explicit authority. Credentials never appear in argv, receipts or logs. Provider guarantees are reported as `enforced`, `best_effort` or `prompt_only` according to observed controls.
