# Fabric v2 contract

Status: design record, 2026-09-23. Source: the reviewed Fabric v2 design in the project research run. The integration gate in that design controls rollout; this document records the durable interface and operating decisions.

## Decisions

Fabric is the default front door for external provider work. A chair uses native subagents for its own models and Fabric for other providers, long runs and worktree writers. Direct CLI remains a named degraded path. Fabric must keep the happy path terse, warn when a runnable route needs substitution, and give a typed terminal state instead of hanging quietly. One supervisor owns each attempt and its provider descendants, including children that enter another process group or session. Full output stays in run files.

The silence watchdog re-checks a provider after `lanes.silence_seconds` without stdout or stderr (default 300; profile knob range 1–3600). It writes `watchdog.json` beside the attempt's events with process liveness, last event time and last observed worktree write. The receipt's `paths.watchdog` names that file; lane listings and digests show the retained silence duration, liveness and observation time. Filesystem scans are bounded; `worktree_scan_complete: false` means the write timestamp covers only the sampled files. The retained `watchdog-streams.json` pointer selects the active capture files across retries and relaunches. It never signals a live provider. Startup and wall-clock deadlines remain separate policies; the retired `CF_DISPATCH_IDLE_SECONDS` setting is ignored. For example, `provenant profile set lanes.silence_seconds=120` changes the observation interval through the existing profile resolver.

On observed provider death, a failed attempt best-effort retains `wip-checkpoint.json` before terminal publication. The abandoned-attempt reaper does the same under run and attempt custody, and observes a still-live orphan without signalling it. The checkpoint holds a bounded Git diff/status summary, the last tool call found in the retained event tail (or null when unavailable), and a recovery next step. Known credential patterns are redacted. The receipt's `paths.checkpoint` is set only after a successful write; a write failure adds a warning and terminal publication continues. The reaper skips Git summaries for read-only lanes and shares one second of Git time across all dead writers in a pass. A missing Git summary or incomplete scan is reported explicitly; it does not prove inactivity.

The supervisor snapshots the provider's descendant tree about once per second during the watchdog wait and again before termination. Linux walks `/proc/<pid>/task/*/children` from the provider and known children when available; macOS reuses one libproc handle. It retains PID plus process start time, so an observed child remains attributable after reparenting without treating a reused PID as the same process. An inherited attempt marker does not prove ancestry: shared hosts can retain it while serving other lanes. A reparented process remains attributable only if its ancestry was observed before reparenting. Cancellation, startup timeout, wall timeout and normal provider exit terminate surviving attributed descendants: SIGTERM, then SIGKILL after 0.5 seconds for descendants and up to 2 seconds for the provider's original group to flush session state. Only after observed provider exit does the supervisor allow up to 1.5 seconds for children closing on stdin EOF. A terminal event from a still-live provider skips that wait. Its direct children that exit within a 0.5-second shutdown window are not reported as leftovers; direct children in another group receive SIGTERM after that window if still alive, then SIGKILL after another 0.5 seconds. It signals attributed descendants individually, never their other process groups, or the direct provider group held by its `Popen` handle. A `codex-code-mode-host` and its subtree are spared even when their ancestry was observed; when the host shares the original group, cleanup uses individual PIDs for that group too. On Linux it best-effort enables child subreaping for adopted orphans. A normal exit that left children running records `reaped: [{pid, command}]` in the attempt and adds `! reaped N leftover process(es)` to the digest. A failed process census is warned in the attempt and cannot prevent the direct provider group kill or terminal record. A nested Fabric owner is spared with its observed descendants only when its `PROVENANT_RUN_DIR/dispatch-owner.json` matches that live process's PID, start time and `PROVENANT_RUN_TOKEN`; tracked children are rechecked before signalling to cover fork followed by exec. The outer provider does not inherit the run token. A nonzero `spared` count is recorded in the outer attempt. A child that enters another session and reparents before its ancestry is observed cannot be attributed safely by this polling supervisor; eliminating that race requires an operating-system process containment facility.

The core MCP surface includes `fabric_dispatch`, `fabric_status`, `fabric_cancel`, `fabric_output`, `fabric_adapters`, `fabric_whoami`, `fabric_send`, `fabric_inbox`, `fabric_acknowledge`, `fabric_note`, `fabric_activity`, and `fabric_task`. These are registered by `runtime/fabric/src/server.ts`.

| Tool | Registered request |
|---|---|
| `fabric_dispatch` | One top-level `prompt` or `prompt_file`, `tasks[]` (1–64, `concurrency` 1–8; above 8 clamps with a warning), `resume` with a new prompt, `handoff` with a new prompt, or `session` (a name) with a new prompt and optional `fresh`. Optional route and control fields include `adapter`, `alias`, `model`, `effort`, `mode`, `worktree`, `cwd`, `network`, `sandbox`, `capabilities`, `add_dirs`, `fallback`, `allow_secrets`, `context_ceiling`, `task_id`, positive finite `timeout_seconds`, `wait_seconds` (non-negative integer; values above 55 clamp to 55 with a warning), and `detail`. With `resume` or `handoff`, `task_id` selects one task of a batch. |
| `fabric_session` | `action: inspect|list|forget` and `name` for inspect and forget. |
| `fabric_status` | `ids[]` of run, task or batch IDs, non-negative integer `wait_seconds` (values above 55 clamp to 55 with a warning), `until: any|all`, and `detail`; `id` is also accepted for one run. One row per run. |
| `fabric_cancel` | Required `id`, optional `reason`; asks the owner to stop the provider and tracked descendants. |
| `fabric_output` | Required `id`, optional `part: result|stderr|events|receipt`, `offset`, and `max_bytes` (positive integer; values above 20,000 clamp to 20,000 and warn); returns a bounded chunk and `next_offset`. |

A worker question yields `input_required`; `fabric_dispatch` with `resume` appends an attempt to the same run. `resume` takes a run ID, a run ID plus `task_id` for one task of a batch, or the task's own ID. A `task_id` with no prior attempt is rejected as `resume_task_unknown`. A resume reuses the prior capability list exactly. It may change `context_ceiling`, the attempt's `timeout_seconds`, and `allow_secrets` for the new prompt; any other route or control change is rejected naming the field, and needs a new dispatch. For a batch, use `fabric_dispatch` with `tasks[]`; wait on returned IDs with `fabric_status`.

A named session is an alias over that resume path ([ADR 0022](../adr/0022-thin-fabric-mcp-execution-facade.md#amendment-named-sessions)). `fabric_dispatch` with `session: "<name>"` starts an ordinary dispatch when the name has no clean turn and otherwise resumes the name's run and task, from any agent in the project. Names are case-sensitive and unique per project. The alias records the actual adapter (`provenance.transport`), the provider-native `session_id`, and the run, task, attempt and result path of the last clean turn (`ok` or `input_required`); no other outcome moves it. A resume continues from that attempt (`--resume-attempt`) and requires exactly its provider session (`--require-session`), so a later failed attempt never redirects it. A turn is one owner invocation: fallback attempts belong to it, and the pointer moves to the attempt that ended it cleanly. One turn runs per name: a second call, or `forget`, gets `session_busy` with `active_run_id`, and a claim planned from a turn another caller has since taken is refused the same way. The turn records its run and first attempt before the owner starts; if that write fails, nothing is launched and the reply is `session_unrecorded` with `session_error`. On every read busy follows the run's lifecycle: the turn is active while its launching process lives, then while its run's task is open or the run's owner lives (an owner whose identity cannot be verified counts as alive). `dispatch-status.json` records the launcher (`host_pid`, `host_started_at`) and, after the spawn, the owner (`owner_pid`, `owner_started_at`, `run_token`), so status keeps a run open while that owner, or the provider it recorded, lives without an owner record; a retained owner record and that stamp both count. An owner that fails to start closes its attempt as `rejected` (`owner_start_failed`); an attempt whose launcher died before starting any owner reads `interrupted` in status, for the name, resume and handoff alike, with the pointer kept on the last clean attempt. There is no timer, heartbeat or queue. Copilot, a last clean turn that recorded no provider session ID, or a provider that no longer has the session (Claude's "no conversation found") returns `continuation_unsupported` instead of relaunching; with `fresh: true` the turn is a `handoff` from the last clean attempt's result and the name moves to the new run once it ends cleanly. `forget` removes only the alias; run files and provider history remain.

Dispatch scans prompt text, prompt files and eligible regular files in `add_dirs` before provider launch. A high-signal secret finding rejects with `error: secret_detected` and a one-line removal or `allow_secrets: true` fix. The boolean override applies at the top level or per task, and an attempt records the override and finding names. Directory scans read each file whole and skip Git-ignored files, `.git`, `node_modules` and other vendored or build directories, binary files (a NUL in the first 8,000 bytes), and directories listed under `secret_scan_exclude` in `<workspace_root>/.agents/fabric-policy.json` (each with a warning); an entry must resolve, links included, inside the workspace root. A scan the budget cannot finish never passes in part: past 10,000 files or 64 MiB the dispatch rejects with `secret_scan_budget_exceeded`, naming the deepest subtree holding at least half the scanned bytes, else the largest top-level one. The same policy's `dispatch_defaults` may set `add_dirs`, positive `timeout_seconds` and boolean `network` (Codex routes only); an explicit dispatch, batch or task field wins, and an invalid key warns and is ignored.

A run has `queued`, `running` and attempt-terminal states. Terminal statuses are `ok`, `quarantined`, `partial`, `failed`, `usage_limited`, `rate_limited`, `auth_required`, `model_unavailable`, `permission_blocked`, `stalled`, `startup_timeout`, `writer_changed`, `timed_out`, `cancelled`, `interrupted`, `rejected`, `tool_missing`, and `input_required`. A provider that writes nothing to stdout within `CF_DISPATCH_STARTUP_SECONDS` (default 300) of launch ends as `startup_timeout` with signature `startup_watchdog`, and its process tree is stopped; stderr banners do not count as output, and time queued for memory admission comes before launch. It is retryable, except for a `worktree_write` attempt that was not provably untouched. Such an attempt falls back only when the worktree was clean at launch (`git status --porcelain`) and, at the timeout, has the same HEAD and status. A bounded walk must also find no file or directory, ignored ones included, whose mtime or ctime is at or after one second before launch. The walk skips `.git`, `.agent-run`, `node_modules`, `.venv`, `__pycache__`, `.pytest_cache`, `dist` and `build`. It counts as changed at 200,000 entries, 5 seconds, or an unreadable directory. Otherwise the receipt says to inspect the worktree instead of falling back. Structured provider events take precedence over text signatures. Fallback creates another attempt under the same run id. Alias routes default to fallback through allowed paid non-training routes; an explicit model defaults to no fallback. Free or prompt-training routes require explicit opt-in.

Before each new attempt starts its provider, the owner compares available host memory with total physical RAM: free, inactive and speculative pages from macOS `vm_stat` using its reported page size against `sysctl -n hw.memsize`, or Linux `MemAvailable` against `MemTotal`. The default floors are 10% for `worktree_write` and 5% for `read_only`; `<workspace_root>/.agents/fabric-policy.json` may set either value under `memory_floor_percent` to a number from 0 to 100, with 0 disabling that mode's floor. Below the floor, the attempt remains `queued` and is checked every 15 seconds. Owners serialise this decision with a per-user host lock at `$XDG_STATE_HOME/provenant/admission.lock` (default `~/.local/state/provenant/admission.lock`), held until 20 seconds after provider start or attempt end; lock creation failure admits with a warning, while a failed or unknown probe holds and retries. Within a project, queued attempts are admitted in order of queue entry: each queued receipt publishes its `queued_since` time, floor and owner identity, and an attempt yields only to an earlier live waiter whose floor the current reading meets. Waiters republish at least once per poll. Ordering is best effort and liveness wins: a receipt holds no place when its owner has exited, its process start time no longer matches (for example after a reboot), or it has not been republished for three polls (a zombie, suspended or stuck owner) or is dated more than a few seconds ahead of the clock. Receipts are scanned only while the host lock is not held. `fabric_status` and `fabric status` show available percentage, floor and elapsed/total wait, which is bounded by that attempt's `timeout_seconds`. On expiry, the attempt fails with error code `memory_unavailable` and a fix. Queued time does not count against execution timeout; cancellation remains available. The parent also excludes published queued time from its child-owner watchdog. An invalid floor records a failed attempt with a one-line fix. Admission only affects new attempts and does not stop running providers.

New attempts in either access mode also require at least 500 MiB free on the run directory's volume, rechecked after memory admission. Set `disk_floor_bytes.worktree_write` or `disk_floor_bytes.read_only` in the same policy file to a non-negative integer to override that mode's floor; both default to 500 MiB and 0 disables the floor. A failed disk probe admits with a warning; an invalid disk policy fails with a fix. Below the floor the attempt fails as `host_disk_full` without starting a provider. Running providers keep going when progress publication or capture writes fail; the owner retains a warning and retries progress publication. An unobserved exit after provider start is `provider_lost` with error class `provider_error`. ENOSPC is `host_disk_full` with error class `host_resource`, including before provider start. Terminal evidence retains the process group, last progress, error excerpt, process-group probe, last event time and log byte offsets. Reaping recovers missing process groups from retained provider records, notes disk-full evidence and closes an existing `dispatch-status.json`.

## Session context

Many routes have 1M-token windows, so resuming a large session can cost far more than starting fresh. Fabric measures each attempt's context, caps it where the provider allows, and warns rather than blocks.

Each attempt records `context: {context_tokens, input_tokens, output_tokens, cached_input_tokens, cache_write_tokens, reasoning_tokens, context_window_tokens, context_percent, source}`. `context_tokens` is the session's current size after the turn. `cached_input_tokens` records cache reads, `cache_write_tokens` records cache writes, and `reasoning_tokens` records reported reasoning or thinking tokens. These are breakdowns of provider usage, not extra tokens to add to existing input/output totals. Fields the provider did not report stay `null`, including cache writes and reasoning; an explicit zero stays zero.

Input totals retain their adapter-specific meanings:

| Adapter | `input_tokens` | Cache writes | Reasoning |
|---|---|---|---|
| claude | `result.usage.input_tokens` + cache creation + cache read; includes cached tokens | `cache_creation_input_tokens` | `output_tokens_details.thinking_tokens` |
| codex | `turn.completed.usage.input_tokens`, passed through including cached input | `cache_write_input_tokens` | `reasoning_output_tokens` |
| cursor | `result.usage.inputTokens` + `cacheReadTokens` + `cacheWriteTokens`; includes cached tokens | `cacheWriteTokens` | not reported |
| opencode | sum of each step's `input` + `cache.read` + `cache.write`; includes cached tokens | sum of `step_finish.part.tokens.cache.write` | sum of `step_finish.part.tokens.reasoning` |
| agy | `result.usage.input_tokens`, passed through; `cache_read_tokens` is recorded separately without adding it to input | not reported | `thinking_tokens` |
| kiro, copilot | not reported (`null`) | not reported | not reported |

For agy the receipt preserves the provider's input total; the retained zero-cache sample does not establish whether its input includes cache reads. Consumers must not assume a uniform uncached-input definition across adapters.

`context.source` is `observed` when the provider reported its latest request, `estimated` when only turn totals exist (these overstate context when a turn made several requests), and `null` when nothing was reported. The field paths come from live turns retained as `tests/fixtures/fabric-context/`:

| Adapter | Context source | Window | Ceiling control |
|---|---|---|---|
| claude | last request usage (`result.usage.iterations`), `observed` | the answering model's `modelUsage` `contextWindow` | `--autocompact <n>` below its compaction point |
| codex | rollout `token_count.last_token_usage`, `observed`; the stream's `turn.completed` totals alone are `estimated` | rollout `model_context_window` | `-c model_auto_compact_token_limit=<n>` below its compaction point |
| cursor | `result.usage` turn totals, `estimated` | not reported | none, `unsupported` |
| opencode | last `step_finish` `tokens.total`, `observed` | not reported | none, `unsupported` |
| agy | last `step_update` usage, `observed` | not reported | none, `unsupported` |
| kiro | `metadata.contextUsagePercentage` only, so `context_percent` | not reported | none, `unsupported` |
| copilot | nothing reported, all `null` | — | none, `unsupported` |

`context_ceiling` is an optional token count. Its default comes from `context.ceiling_tokens` in `config/model-routing.json` (300,000). A value outside 100,000–1,000,000 is clamped with a warning, never rejected. The ceiling only lowers a provider's compaction point; it never raises it. Codex compacts at `effective_context_window_percent` of the model's `context_window` in `$CODEX_HOME/models_cache.json` (about 258k for a 272k model). Fabric never passes `model_context_window`. Claude compacts at the model window: 1M for `opus`, `sonnet`, `fable` and `haiku` (Haiku 5.5), 200k for an explicit `claude-haiku-4-5`. An explicit Haiku 4.5 route remains bounded by its 200k window even if Claude answers with a larger-window model. If the user's `autoCompactWindow` in `~/.claude/settings.json` is lower, Claude compacts there instead: dispatched `--safe-mode` runs load user settings. The flag is passed only when the ceiling is below that point. When the point is unknown (a model missing from the Codex cache, no cache, or an unlisted Claude model), no flag is passed, because lowering cannot be proven. The attempt records `provider_default` with a `null` point and warns once.

The attempt records `applied.context_ceiling` as `enforced`, `provider_default` (the provider's own point is at or below the ceiling, with `applied.context_ceiling_source`) or `unsupported`. It also records `applied.context_ceiling_tokens`, the point in force (`null` when unknown), and `applied.context_ceiling_requested`. A resume inherits the prior requested ceiling unless the call passes a new one.

On macOS, Codex providers in `read-only` and `workspace-write`, and every provider under a Fabric `sandbox-exec` profile, resolve `ps` to the bundled libproc shim through PATH. The shim and `process_info.py` are staged in `<attempt>/tmp/provenant-shim`, because the product checkout may sit below a denied home or outside a read-only lane's `cwd`. The setuid `/bin/ps` cannot execute under any sandbox. The shim covers the forms agents type (`ps aux`, `ps -ef`, `ps ax`, `-p`, `-o`/`-O` with common fields) and names its supported subset when asked for more. Its `lstart` is `strftime("%c", localtime(start))` in the caller's locale, padded to 28 bytes on macOS, matching `/bin/ps` byte for byte; when libproc cannot read a live PID, `sysctl(KERN_PROC_PID)` reports its PID, parent, group, user, start and elapsed times. A missing PID remains gone. Linux retains `/proc` and normal `ps` behaviour. This PATH adjustment adds no receipt field.

A resume reads the prior attempt's context. It adds a digest warning when that context exceeds the effective ceiling (the point in force, else the requested ceiling), or when the size is unknown and the adapter has no ceiling control, for example `! resuming a ~620k-token session; fresh: fabric_dispatch{prompt, handoff:"<run id>"}`. The resume still runs. `handoff: <run id>` (with `task_id` for a batch task) is the cheaper alternative. It starts a fresh run whose prompt is prefixed with the prior task's route line and its result tail, at most 8,000 bytes in total. If the call names no adapter, alias or model, the handoff reuses the prior adapter, model and effort, and a prior writer's mode and worktree. The prior task must be terminal.

Host liveness probes treat permission denial and unavailable start times as unknown, so they exclude orphan reaping. Only a missing process or a verified identity mismatch proves host loss. Signal control verifies each recorded process and its actual group, then signals only the owner and provider PIDs; their owner handles descendants. A partial process census cannot authorise controller group signals. Before signalling, it writes `termination-request.json` with the run token, target provider, source, signal and requester PID. An unexpected provider signal death without a valid terminal result reports `failed`; a signal after a valid terminal result retains `interrupted`. Both retain the numeric signal and name; matching controller telemetry supplies the termination source and requester. The actual OS signal sender remains unknown. Watchdog, cancellation and terminal-grace cleanup keep their own outcomes.

A terminal digest appends a compact marker to its Route line: `ctx 212k/1M` when observed, `ctx ~19k` when estimated, `ctx 8%` from a percentage, and nothing when unknown. The stored provenance line stays unmarked for trailers and the index. Brief status rows omit `context`; `detail: "full"` includes it.

## Receipt and provenance

Each attempt records the sorted list in `applied.capabilities`; an omitted or
empty list is `[]`.

An owner writes `fabric.attempt.v1` at `tasks/<id>/attempt-NNN/attempt.json` and aggregates it into `RUN_RECEIPT.json`. A status row is `fabric.status.v1`: the latest attempt plus attempt history, batch id where relevant, and live worktree ledger fields. The attempt records state, status, mode, cwd, workspace_root, worktree, timing, process group, session id, retry fields, evidence, question, applied sandbox/network/additional directories/confinement/guarantee/context ceiling, context, warnings, provenance, paths and digest. `workspace.cwd` is the provider's actual cwd; `workspace.root` remains the caller workspace root. Dispatch attempts, batch task rows and run receipts write `ok` for success. Readers accept `succeeded` in retained older receipts and present it as `ok`. Unknown fields may be ignored; removals bump the version.

Every attempt has `usage_source`: `observed` when token counters were read from the provider stream, `estimated` when usage was estimated, or `missing` when no token counters were reported. Current adapters do not estimate token usage; `estimated` is reserved for an estimator and preserved when supplied by an adapter. A context percentage alone is `missing` usage. This field is independent of `context.source`: Codex/Cursor turn totals are observed usage even when their context size is estimated. The meter result is retained on unsuccessful attempts, including non-zero provider exits.

Terminal adapter, dispatch and Fabric attempt receipts also record `error_class`, derived only from that receipt's final status and existing evidence signature. The fixed vocabulary is `none`, `rate_limited`, `usage_limited`, `auth`, `model_unavailable`, `timeout`, `empty_output`, `sandbox_denied`, `provider_error`, `host_resource`, `transient`, `cancelled`, and `unknown`. Successful and input-required statuses map to `none`; named failure statuses take precedence over signatures. Signatures refine generic outcomes (for example `failed` + `empty_output`). Stalls and startup deadlines map to `timeout`; permission blocks map to `sandbox_denied`. Generic failures, rejected requests and missing tools map to `provider_error`; cancellations map to `cancelled`; interruption, quarantine and unrecognised outcomes default to `unknown`. Transport and exhausted reconnect failures map to `transient`. Codex progress text without a terminal result is a failure. Retained incomplete output carries `result_partial: true` in adapter and attempt receipts, lane rows and brief status responses; lane waits label it partial. A successful terminal event clears preceding reconnect errors. This telemetry field does not affect routing, retries or health.

`timing.phases` records milliseconds for available dispatch phases: TypeScript validation and catalogue snapshot, run directory setup, owner startup and attempt setup, route planning, provider spawn, provider execution and finalisation. A direct Python owner has no TypeScript phase measurements. Writer progress scans at most 2,000 entries per probe, checks a 25 ms budget between entries and retains its scan position; a full cycle on a large tree may take several one-second probes. Incomplete scans never authorise a stop, and silence observations do not depend on writer progress.

Provenance records requested route, resolved and observed model, observation source, identity (`observed`, `resolved`, `unknown`), provider, transport, family, requested and applied effort, CLI version, fallback origin, the installed Provenant revision (`harness_rev`, null outside a Git checkout), notes and the generated line. `effort_applied` is the effort sent to the provider, clamped to the model's supported list. It is empty when none was sent: the model has no effort control (the catalogue gives it no `efforts` list, or `effort_transport: "none"`), or nothing was requested and there is no alias default. It never repeats an unsent request or guesses a provider default. The one exception is an effort the provider itself reports: with none sent, Codex's rollout `turn_context` fills it and `effort_observed_source` names that source; otherwise `effort_observed_source` is null. An ignored request stays in `effort_requested` and adds the note `effort <x> ignored: <model> has no effort control`, shown once among the digest warnings. A terminal digest and status row carry `Route: adapter/model@effort (provider; identity)`, with no `@effort` suffix when applied is empty; the retained index stores that line after run pruning. The proposed `provenant route <run-id>` index lookup is not yet wired: the current command invokes the model catalogue router. Until it is, copy the line from the digest or status row and use `Agent-Route: adapter/model@effort` for the commit trailer. Never infer a model from a worker self-report. Read-only Claude runs use `--permission-mode default`, `--permission-prompts none`, `--tools Read,Grep,Glob,Bash` and `--allowedTools Bash`, so reviewers can diff, run targeted tests and reach the network. The lane's OS confinement keeps writes inside the attempt; the shell can read the lane's environment and reachable files, an accepted trade-off. They no longer use plan mode. Claude's observed model is the one on the assistant messages that answered (`claude:assistant.message.model`), else a lone `result.modelUsage` key, and `system.init.model` only when neither exists. Provenance keeps this comparison because Claude can still substitute under other invocations. For a substituted model, the family comes from catalogue patterns only when exactly one family matches; absent or ambiguous matches stay `unknown`, and substitutions cannot set `cross_family` or `certification_eligible`. Claude Code 2.1.280 in plan mode reports `claude-haiku-4-5` in init but `claude-sonnet-5` (1M) answers; Opus stays Opus, and a writer's explicit Haiku 4.5 answers as Haiku 4.5 (200k). The attempt records `init_model` and `answered_models` and warns `claude answered as <x>; init reported <y>`. If several models answered, the final top-level one goes on the Route line. Native Claude subagents use the Agent tool model parameter and record `claude/<model>@<effort> (anthropic; resolved)`.

## Layout and retention

Run root is the primary checkout root, including when dispatch starts inside a linked worktree. Runs live at `.agent-run/runs/<YYYYMMDD-HHMM>-<kind>-<slug>-<rand6>/`; owner logs live in `_owner/`. The legacy top-level entries (`sessions/`, `locks/`, `scratch/`) remain under the run owner; Fabric grants every write-confined lane write access to `locks/` only. New [session checkpoints](../../skills/session/references/chair-state.md) use explicit project/host-session keys under the fresh workflow state root. `.worktrees/` names derive from branches with `/` replaced by `-`. New repositories exclude `.agent-run/`, `.worktrees/` and legacy `.work/` from Git locally.

`provenant clean` plans by default and prints a digest. the plan is saved beside its digest under `.agent-run/scratch/` (it expires with scratch after one day, after which `--apply` fails and `provenant clean` must be rerun), and `--apply --plan sha256:<digest>` (with the same `--include` and `--older-than`) verifies that file, replans, and deletes only approved rows that are still exactly as approved, each reclassified and identity-checked again immediately before its anchored, no-follow deletion. Rows that vanished or changed are skipped and reported; rows that became eligible after approval are never touched, so lanes may keep running between plan and apply. A changed integration branch or worktree proof still aborts. `runs/index.lock` is kept infrastructure; clean merged worktrees can be removed without extra authority; dirty worktrees and unmerged work are never removed. Live, pinned, cited, unaccepted delivery, resumable and active mission runs are protected. Here `unaccepted` describes the retained receipt-code test, not a universal user-approval requirement; [issue #1025](https://github.com/mblauberg/provenant/issues/1025) records the pending code alignment. A terminal dispatch attempt prunes its private `tmp/` at once (kept for `input_required`, for a failed attempt under 50 MB, or when `PROVENANT_KEEP_ATTEMPT_TMP=1`); `clean` also removes `tmp/` and `cache/` of retained attempts after one day for successful runs and seven for others, keeping receipts, results and logs for the run's retention. Sizes count allocated blocks once per inode and never follow links. Successful and cancelled runs retain seven days; typed failures retain fourteen; accepted deliveries retain thirty; owner logs retain seven; scratch retains one day. Sessions are durable handoffs and remain triage-only. Unknown paths are triage-only. If GitHub PR state cannot be read, the plan warns and holds run deletion; ancestry-proven merged worktrees can still expire. Worktrees require clean state and merge proof: ancestry for merge commits, or a merged PR whose head SHA matches the local branch tip for squash merges, then removal through `scripts/worktree remove`. The append-only run index keeps provenance for at least 365 days. Readers use `.agent-run/runs/<timestamped-name>`; root-level `mcp-*`, orchestration, delivery and mission layouts are no longer read.

Provider instruction paths remain provider-owned. The Rust release's `provenant install` command applies the compiled harness instruction and link plan, preserving unrelated entries and refusing unlisted paths. A cut-over uses an explicit fresh state root, selected harnesses and `--legacy-checkout` for exact legacy matches. Keep repository `AGENTS.md` project-specific; relying on an installed instance entrypoint reduces repeated context but makes the checkout less self-contained when opened without that harness.

## Routing and authority

A read-only `cwd` or a `prompt_file` outside the caller's directory must lie in
a registered Fabric project; otherwise the rejection names registering that
project or dispatching from it. Fabric passes each such directory to the owner
as a read root, which the owner accepts beside its workspace, records in the
attempt as `read_roots` and restores on resume. A resume refuses
(`resume_read_root_changed`) when a saved root or its cwd now resolves elsewhere
or, in Fabric, has left every registered project. The run stays in the caller's
run root. A prompt path the credential-store rule matches is refused in any
root.

`.agents/fabric-policy.json` declares `protected_paths` relative to the
directory containing `.agents/`; Fabric discovers it at the workspace root and
the Git toplevels of workspace, cwd, worktree and read roots, plus immediate child
repository toplevels of a non-Git workspace, and mirrors repository paths into
every registered worktree. Routes resolve `trains_on_prompts` from the model,
then the adapter; an unresolved value counts as training. A training route is
rejected when its prompt file or additional directory overlaps a protected
path, its cwd lies inside one, or OS read confinement is unavailable. Its
`sandbox-exec` profile
denies reads of those paths in every registered worktree. Non-training routes
are unaffected. Codex writer confinement is supplied by a permissions
profile named uniquely for each plan (`-c default_permissions="provenant-<random>"`,
extending `:workspace`, on fresh runs and resumes), because Codex merges config
tables and a fixed name would inherit a system config's grants. Its
`filesystem` table grants `:tmpdir`, `add_dirs` and the Git write boundary below
and keeps the common directory and the worktree `.git` marker read-only; a fresh
run also passes `--add-dir` and `--cd <worktree>`. Codex keeps a writable root's
`.agents/` read-only, even an absent one, which stops a rebase or merge that
updates or adds a tracked skill, so a linked-worktree Codex writer also gets the
worktree's `.agents/` in `add_dirs` unless it is a file or link; an absent one
is created for the attempt and removed afterwards if still empty. After every
writer attempt, whatever the adapter, each `.agents/` path in HEAD, the index
and on disk must match the attempt's starting HEAD, index or files, or the
primary checkout's branch or its upstream; a path that branch changed and the
lane merged or rebased onto must keep the branch's version, so a merge that
discards it (`git merge -s ours`) is caught, except where the lane's branch had also changed that
path before the attempt: its starting version then stands. A starting untracked file the lane removes or hides
behind a link counts as changed. Files on disk are hashed without Git, so ignored, skip-worktree and
filtered files count. An unresolved conflict, an unreadable directory, a
special file, a replaced `.agents/` root or a tree over 256 MiB blocks landing.
Spared processes do not imply instruction edits: the check still compares
HEAD, index and disk against the starting and integration references. An absent `.agents/` counts as empty, since Git
removes the directory with its last file; a link or file in its place blocks landing.
Otherwise `instruction_changes` in `<workspace_root>/.agents/fabric-policy.json`
decides. The default `quarantine` archives the lane's files as a binary patch
at `<attempt>/protected.patch`, plus `protected.index.patch` or
`protected.head.patch` when its index or HEAD held different content, each
published through the attempt directory's descriptor with an atomic replace
that never follows a planted link. It then commits the start or
integration-branch version of each path back to the lane as Provenant Fabric,
returns the index and files to their starting content (or the integration
branch's version), rechecks, and ends `quarantined` only when the provider
succeeded and restoration was verified (`restored: true`); starting files Git did
not hold are stored as objects before launch so they can be restored. `allow`
keeps an ordinary edit with a warning. When edits remain on the lane branch or
worktree (`deny`, a failed restore, or a conflict, special file, case variant or
unverifiable state quarantine cannot express) the lane ends `failed` as without
quarantine, with `fix: instruction edits remain on the lane branch; restore
.agents/ to the start commit before landing`; its output and patches stay
retained and listed. A `quarantined` lane retains the result, sets
`landable: false` and `retryable: false`, and records `quarantine` with the
reason, patch paths, whether restoration succeeded and any patch capture error.
`landable: false` is never `ok` for landing, certification or session success.
Digests, status and
lanes show the result and a review action; chairs review the quarantine and
verify the branch diff before landing any application work. Proposed instruction
edits belong outside `.agents/` for separate chair review, as the default writer
brief states (omitted when the policy is `allow`).

The per-attempt check above is an early warning. Two owner-side controls
enforce the hold, so neither a surviving provider child committing after
finalisation nor a moved local ref can land an instruction change:

- **Writer ref grants.** A writer's Git may write its private worktree
  directory, the object store and only its own branch: `refs/heads/<branch>`,
  its `.lock` and its reflog. The owner reads the branch from the worktree's
  HEAD and refuses one that the primary checkout or another worktree holds; a
  detached writer gets no shared ref. No writer can move the primary branch, its
  upstream, a tag, `packed-refs` or another checkout's branch. Every boundary,
  native and sandbox-exec, also keeps `.git`, the private `commondir`, `gitdir`
  and `config.worktree`, and the common `objects/info` read-only.
- **Range gate.** `lane_protection.py` walks every commit in `tip ^base` through
  the owner's common Git directory with `diff-tree --root -m`, so merge
  resolutions count. A commit may change a protected path only to the version
  the base already holds, as a merge or rebase from the base does, or be
  reachable from a tip the chair released. A merge may also take a protected
  path's version from a parent in the range that is not held, but only where a
  released commit in the range introduced that version. A landing train's
  `--no-ff` merge of a released tip, and later merges on top of it, therefore
  pass. A merge cannot revert a protected path to an older version taken from a
  parent outside the range or from a stale branch, and a resolution no parent
  holds is held. One exception remains: a released commit vouches for the
  parent content it merges, so a later unreleased merge in the range can
  restore that stale content, and the gate passes it. A release is therefore
  a review of the whole released tip, parents included. A release record that
  is malformed fails every range, protected-clean ones included, rather than
  only ranges that change protected paths. Anything else fails with
  `protected_instructions_changed` and names the held commits; the branch is
  left as it is. Fabric `landing-push` passes the landing lease's remote SHA as
  the base, never a local ref. `scripts/worktree verify-claim` and a manual
  check use the primary checkout's HEAD, which no writer can move. The gate
  needs no per-attempt record, so chair, human and never-dispatched worktrees
  go through the same check.

The invariant is: nothing lands on the integration branch unless every commit in
`<leased remote SHA>..<tip>` leaves protected paths unchanged from the leased
version, is reachable from an audited chair release of a reviewed tip, or is a
merge that takes each protected path from the leased version or from a released
change in the range;
writers can write only their own branch ref. The gate reads the object store,
which writers can add packs to, so it trusts object content; it disables
replace refs, commit graphs, the multi-pack index and lazy fetch, and refuses
alternate object stores. Path classification normalises to NFC before
casefolding: casefold covers APFS case folding, but not Unicode normalisation
by itself.

A quarantine restores the files in a new commit, so the edit stays in the
lane's history and the gate holds it. After reviewing the exact commits, a
chair outside a writer either rewrites the lane without them or releases the
reviewed tip:

```sh
python3 "$(provenant root)/skills/orchestrate/scripts/lane_protection.py" \
  --cwd <lane-worktree> --release --tip <reviewed-sha> \
  --actor <chair> --reason '<review decision>'
```

Release requires the full SHA, refuses when the checkout's HEAD has moved
past it, and writes an immutable record of the tip, actor, reason, worktree
and time under the common directory's `provenant-instruction-checks/releases/`.
It covers the tip and its ancestors, never a later commit. The record is an
audit trail, not authorisation: anyone who can write the owner's common
directory could add one. No writer boundary grants that directory, and the
command refuses to run under a writer attempt's marker, so release stays
chair-side.

`protected.patch` is retained even for an index-only change (empty disk patch)
or an unverifiable check (a comment explaining that the diff is unavailable).
If that marker cannot be installed, patches are listed as unavailable with a
capture error; no patch is claimed as complete in that case. The check
reports `protected_instructions_unverifiable` separately from actual changes.
A failed or signal-interrupted provider is never promoted to `quarantined`.
Signal-killed attempts (`failed` without a terminal result, otherwise `interrupted`, or `cancelled` on explicit cancellation) retain their changed files without restoration and
fail for actual instruction edits, preserving the provider exit and signal.
An unknown value warns and quarantines. The per-attempt check reads local refs; only
the owner can move them. It also reports a clean three-way merge into a skill the branch already
changed, a refresh overtaken by a newer integration-branch change to the same
file, a refresh under line-ending conversion, and any refresh in a repository
that tracks a case variant such as `.Agents/`. A separately authorised review
can adopt the saved patch for such a refresh. Review of the branch
diff remains the backstop.

`capabilities` is an optional list with distinct values from `postgres` and
`browser`; an empty list means absent. Non-Codex adapters apply it without a
grant, because their `sandbox-exec` profiles restrict only file access; a
browser lane sets `MAC_CHROMIUM_TMPDIR` to a short private per-attempt
directory, `/tmp/fb-<id>`. A Codex
`sandbox: "full"` lane applies none, with a warning, since it is unsandboxed.
Any other Codex lane needs `worktree_write` with `sandbox: "workspace-write"` on macOS, with usable
`sandbox-exec` outside another sandbox and applied `network: true`. The lane
keeps `applied.sandbox: "workspace-write"` and
`applied.guarantee: "enforced"`, and runs Codex with its native sandbox
disabled inside the OS profile. The profile allows Codex's native Mach
services plus FSEvents, then denies other Mach lookups and registrations,
denies external signals while allowing same-sandbox signals, denies
preference writes through `cfprefsd`, and denies System V IPC by default. `postgres` adds shared-memory and semaphore IPC. `browser`
adds the macOS browser services and Chrome/Chromium rendezvous Mach lookup and
registration prefixes; browser lanes set `MAC_CHROMIUM_TMPDIR` to their
`/tmp/fb-<id>` directory for Chrome's process-singleton socket. These capabilities add no
network access beyond a Codex writer with network enabled; Unix-domain socket
connects are limited to `cwd`, declared `add_dirs`, the attempt directory, task
Codex home and mDNSResponder. SBPL `(local ip "localhost:*")` matches every
local address, so an inbound loopback rule would not establish a loopback limit.

Each task uses one `CODEX_HOME` at `<task directory>/codex-home` for all
attempts. Existing `auth.json`, `AGENTS.md`, `HARNESS.md` and `skills` are symlinked from the
caller's `CODEX_HOME`, or `~/.codex`; the profile grants writes to the task
home and only the literal source `auth.json`. It grants each Git write
boundary path by its own name, so a link planted at one never moves a later
grant. Fabric recreates the task home links every attempt and fails a symlinked or
non-directory task home, while allowing the lane to overwrite the literal
source `auth.json` for token refresh, a file it could already read. That grant
names the unresolved source path and covers in-place rewrites only, not
deleting, renaming or replacing the entry. A symlinked or non-regular source
`auth.json`, or a source Codex home inside a lane-writable path or reached
through a symlink, fails the attempt before launch. Chrome must use `--no-sandbox` because macOS refuses its nested sandbox. PostgreSQL socket
paths under lane `TMPDIR` exceed macOS's
103-byte limit; use TCP or a short socket directory. Attempts set `TMPDIR`,
`TMP` and `TEMP` to `<attempt>/tmp`, zsh's `TMPPREFIX` to `<attempt>/tmp/zsh` (for
here-documents), `XDG_CACHE_HOME` to `<attempt>/tmp/cache` and
`COREPACK_HOME` to `<attempt>/tmp/cache/node/corepack`, `UV_CACHE_DIR` to `<cache>/uv`
and `npm_config_cache` to `<cache>/npm`. Read-only attempts also append
`-p no:cacheprovider` to `PYTEST_ADDOPTS` and set `PYTHONPYCACHEPREFIX`,
`RUFF_CACHE_DIR` and `MYPY_CACHE_DIR` under the cache. A Codex writer's
permissions profile grants `:tmpdir`, so `TMPDIR` stays writable. Codex
read-only runs use a per-plan permissions profile named the same way, which
extends `:read-only` with writes to `:tmpdir` and to
each `add_dir` that neither holds `cwd` nor contains a character Codex reads as a
permission pattern (`*?[]{}`; Codex strips a trailing `/**`), and sets
`network.enabled` to the applied network. OpenCode's
read-only `sandbox-exec` profile reads the project config OpenCode loads at
startup (`opencode.json`, `opencode.jsonc`, `AGENTS.md`, `CLAUDE.md`, `.opencode/`)
between `cwd` and the repository root, granted by unresolved name. Every read-only
`sandbox-exec` profile reads the lane's toolchain, found without running it:
- `python3`, `python`, `uv` and `node` on the provider PATH;
- pyenv shims resolved from `PYENV_VERSION`, the nearest `.python-version`
  (starting at `PYENV_DIR` or `cwd`), then `PYENV_ROOT/version`. Discovery
  never executes a shim or hook. No selection means `system`; a prefix such
  as `3.12` uses the highest installed `3.12.*`. An unresolvable candidate is
  skipped with a warning; unsafe targets fail before launch with a fix hint. Put the selected interpreter's `bin` on
  PATH when a custom pyenv hook controls selection. The shim, selected
  version file and pyenv's real `libexec` directory are readable in the
  confined lane, together with the validated interpreter runtime;
- each `.venv` between `cwd` and the repository root, its `bin/python` link and
  the interpreter its `pyvenv.cfg` names, all treated as candidates only;
- with a granted `uv`, `pyproject.toml`, `uv.toml`, `uv.lock` and
  `.python-version` above `cwd`.

A candidate is granted only if it resolves to a regular executable named
`python`, `python3`, `python3.N` or `node`, each Python name optionally with
the free-threaded `t` suffix. The file must sit in the `bin` of a prefix
holding `lib/python3.N[t]` or `lib/node_modules`. That prefix must be neither
home nor an ancestor of home, and must not pass `credential_path`. Only the
executable and the prefix's real (unlinked) `lib`, `include` and `libexec` are
granted, never the prefix itself; a framework prefix (`<Name>.framework/Versions/X.Y`)
adds its `<Name>` library and `Resources`. Each component must be a real entry,
not a link, strictly below the prefix, so a link to `.` cannot grant it. The profile emits the canonical paths it
checked without resolving them again. A resolved `uv`/`uvx` binary is granted alone. `credential_path` also
covers `.git-credentials`, `.pypirc`, `.pgpass`, `.vault-token`,
`.password-store`, `.boto`, `.s3cfg`, `.terraform.d`, Cargo and Gem credentials,
`.config/git/credentials`, `.config/hub` and `.config/op`.

Every attempt records top-level `account` and `host` before provider startup;
terminal records retain that scope. Quota and cooldown routing share
`scripts/provider_identity.py`. Account identity comes from paths, without
reading credential contents:

| Adapter | Selected identity source |
|---|---|
| claude | `CLAUDE_CONFIG_DIR`, else `~/.claude` |
| codex | `CODEX_HOME`, else `~/.codex` |
| agy | On macOS the OS user's `~/Library/Keychains/login.keychain-db`, independent of HOME and `AGY_CONFIG_DIR`; elsewhere `~/.gemini/antigravity-cli`; a second account requires a second host |
| cursor | `~/Library/Application Support/Cursor/User/globalStorage` on macOS, else `~/.config/Cursor/User/globalStorage` |
| opencode | `$XDG_DATA_HOME/opencode`, else `~/.local/share/opencode` (the authentication store, not `OPENCODE_CONFIG_DIR`) |
| kiro | `~/.kiro` |
| copilot | `~/.copilot` |

A selected path matching an enabled configured
`telemetry.accounts.<provider>.<label>.identity_dir` resolves to `<label>`.
Otherwise the provider's default directory resolves to `default`, and another
resolved path to `path-<sha256 prefix>`. Paths, emails and tokens are never used
as account keys. An unresolved identity is `null` and matches only `null`.
For inspection and reset commands, see [model routing](../runbooks/model-routing.md).
Host is the telemetry host id (`local_host` in `.agent-fabric/hosts.json`, else
hostname). Neither account nor host acts as a wildcard.

`fabric.cooldowns.v1` entries carry `adapter`, `model`, `account` and `host`.
Storage keys are JSON-encoded `[adapter, account, host, model]` tuples, allowing
live scopes to coexist; readers expose only the active scope as `adapter/model`
keys. Exact model entries precede `*` within that scope. Reads prune expired
entries and retain other accounts and hosts. Under `cooldowns.lock`, a legacy
entry without `host` and with a null account migrates once to the provider's
default identity on this host, even if an alternate identity is selected.
A new explicit null scope is never migrated. Cached quota adds and reconciles
cooldowns only for its matching account and host.

An `ok` attempt clears the cooldown for the model that answered in that
attempt's account and host, using the resolved model when the provider reports
no model. It also clears the adapter's
account-wide `*` cooldown when the attempt started after that entry was recorded
(never for per-model adapters `agy` and `opencode`). Other model cooldowns remain
intact; `provenant route reset` clears them explicitly. OpenCode runs with error logs on stderr so a quota
retry cannot silently sleep until the run timeout. Quota diagnostics end the
attempt as retryable `usage_limited`, with `quota_exhausted` evidence.
OpenCode Go cooldowns apply to the exhausted model. A reported reset takes
precedence; a monthly limit without a reset hint uses a 24-hour re-probe window: the
next attempt after expiry is the probe, success clears the entry and failure
re-arms it. OpenCode quotas of any model never widen to the whole adapter. Silence alone
remains a timeout. Kiro's plain `Opening browser...` login diagnostic ends
the attempt as `auth_required`, with `auth_expired` evidence and a hint to
run `kiro-cli whoami` outside the sandbox.

`model_route.py snapshot --json` is the single merged catalogue source. Unknown model IDs pass through with a note when runnable; unsupported effort substitutes to the nearest supported value. Explicit cooling models run with a warning. A hard rejection is reserved for impossible execution or a hard boundary. Per-run flags are preferred; editing global provider configuration requires explicit authority. Credentials never appear in argv, receipts or logs. Provider guarantees are reported as `enforced`, `best_effort` or `prompt_only` according to observed controls. On macOS, read-only agy and OpenCode launches use `sandbox-exec` when available to deny workspace reads outside `cwd` and `add_dirs`, and deny workspace writes. Every writer, Codex included, has one Git write boundary: its per-worktree Git directory (`--absolute-git-dir`) plus the common directory's `objects`, `refs`, `logs`, `packed-refs`, `packed-refs.lock` and `packed-refs.new`; the rest of the common directory (`hooks`, `config`, `info`, other lanes' `worktrees/<name>`) is never writable, and a Codex writer drops an `add_dir` at or inside it with a warning. Codex writers also keep the worktree `.git` marker and the private directory's `config.worktree`, `commondir` and `gitdir` read-only; Codex refuses to launch with a symlinked grant path. Writer attempts set `gc.auto=0`, `maintenance.auto=false` and `rerere.enabled=false` through `GIT_CONFIG_COUNT`. Because the shared `config` is read-only, a lane pushes with `git push origin HEAD` and opens its pull request with `gh pr create --head <branch>`; `git push -u` pushes but cannot record the upstream. Wrapped writer launches (agy, Claude, Cursor, OpenCode and Kiro) restrict writes to the worktree, declared `add_dirs`, the Git write boundary, attempt files, temp paths, devices and provider state. Where protected-path policy applies, its read and write denies take precedence within an `add_dir`; receipts list the writable `add_dirs` in `applied.write_boundary`. Codex writers without capabilities use its native sandbox with that permissions profile, recorded as `applied.write_boundary.filesystem`. Unavailable OS confinement refuses agy write dispatches and produces an explicit warning for other wrapped writers.
