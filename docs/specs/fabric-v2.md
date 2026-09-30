# Fabric v2 contract

Status: design record, 2026-09-23. Source: the reviewed Fabric v2 design in the project research run. The integration gate in that design controls rollout; this document records the durable interface and operating decisions.

## Decisions

Fabric is the default front door for external provider work. A chair uses native subagents for its own models and Fabric for other providers, long runs and worktree writers. Direct CLI remains a named degraded path. Fabric must keep the happy path terse, warn when a runnable route needs substitution, and give a typed terminal state instead of hanging quietly. One supervisor owns each attempt and its provider descendants, including children that enter another process group or session. Full output stays in run files.

The supervisor snapshots the provider's descendant tree about once per second during the watchdog wait and again before termination. Linux walks `/proc/<pid>/task/*/children` from the provider and known children when available; macOS reuses one libproc handle. It retains PID plus process start time, so an observed child remains attributable after reparenting without treating a reused PID as the same process. A unique environment marker inherited from the provider identifies children reparented between snapshots; only newly started candidates adopted by PID 1 or the Linux subreaper are considered, and their identity is checked again before signalling. Cancellation, wall timeout, idle stall and normal provider exit terminate surviving attributed descendants: SIGTERM, then SIGKILL after 0.5 seconds for descendants and up to 2 seconds for the provider's original group to flush session state. Only after observed provider exit does the supervisor allow up to 1.5 seconds for children closing on stdin EOF. A terminal event from a still-live provider skips that wait. Its direct children that exit within a 0.5-second shutdown window are not reported as leftovers; direct children in another group receive SIGTERM after that window if still alive, then SIGKILL after another 0.5 seconds. It signals only attributed processes or the direct provider group held by its `Popen` handle. On Linux it best-effort enables child subreaping for adopted orphans. A normal exit that left children running records `reaped: [{pid, command}]` in the attempt and adds `! reaped N leftover process(es)` to the digest. A failed process census is warned in the attempt and cannot prevent the direct provider group kill or terminal record. A nested Fabric owner is spared with its observed descendants only when its `PROVENANT_RUN_DIR/dispatch-owner.json` matches that live process's PID, start time and `PROVENANT_RUN_TOKEN`; tracked children are rechecked before signalling to cover fork followed by exec. The outer provider does not inherit the run token. A nonzero `spared` count is recorded in the outer attempt. On macOS, a child that clears the inherited marker and reparents before the first snapshot cannot be attributed safely by this polling supervisor; eliminating that race requires an operating-system process containment facility.

The default MCP surface has twelve tools: `fabric_dispatch`, `fabric_status`, `fabric_cancel`, `fabric_output`, `fabric_adapters`, `fabric_whoami`, `fabric_send`, `fabric_inbox`, `fabric_acknowledge`, `fabric_note`, `fabric_activity`, and `fabric_task`. These are the tools registered by `runtime/fabric/src/server.ts`; `FABRIC_LEGACY_TOOLS=1` also registers `fabric_batch`, `fabric_team_create` and the earlier task tools.

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

A run has `queued`, `running` and attempt-terminal states. Terminal statuses are `ok`, `partial`, `failed`, `usage_limited`, `rate_limited`, `auth_required`, `model_unavailable`, `permission_blocked`, `stalled`, `startup_timeout`, `timed_out`, `cancelled`, `interrupted`, `rejected`, `tool_missing`, and `input_required`. A provider that writes nothing to stdout within `CF_DISPATCH_STARTUP_SECONDS` (default 300) of launch ends as `startup_timeout` with signature `startup_watchdog`, and its process tree is stopped; stderr banners do not count as output, and time queued for memory admission comes before launch. It is retryable, except for a `worktree_write` attempt that was not provably untouched. Such an attempt falls back only when the worktree was clean at launch (`git status --porcelain`) and, at the timeout, has the same HEAD and status. A bounded walk must also find no file or directory, ignored ones included, whose mtime or ctime is at or after one second before launch. The walk skips `.git`, `.agent-run`, `node_modules`, `.venv`, `__pycache__`, `.pytest_cache`, `dist` and `build`. It counts as changed at 200,000 entries, 5 seconds, or an unreadable directory. Otherwise the receipt says to inspect the worktree instead of falling back. Structured provider events take precedence over text signatures. Fallback creates another attempt under the same run id. Alias routes default to fallback through allowed paid non-training routes; an explicit model defaults to no fallback. Free or prompt-training routes require explicit opt-in.

Before each new attempt starts its provider, the owner compares available host memory with total physical RAM: free, inactive and speculative pages from macOS `vm_stat` using its reported page size against `sysctl -n hw.memsize`, or Linux `MemAvailable` against `MemTotal`. The default floors are 10% for `worktree_write` and 5% for `read_only`; `<workspace_root>/.agents/fabric-policy.json` may set either value under `memory_floor_percent` to a number from 0 to 100, with 0 disabling that mode's floor. Below the floor, the attempt remains `queued` and is checked every 15 seconds. Owners serialise this decision with a per-user host lock at `$XDG_STATE_HOME/provenant/admission.lock` (default `~/.local/state/provenant/admission.lock`), held until 20 seconds after provider start or attempt end; lock creation failure admits with a warning, while a failed or unknown probe holds and retries. Within a project, queued attempts are admitted in order of queue entry: each queued receipt publishes its `queued_since` time, floor and owner identity, and an attempt yields only to an earlier live waiter whose floor the current reading meets. Waiters republish at least once per poll. Ordering is best effort and liveness wins: a receipt holds no place when its owner has exited, its process start time no longer matches (for example after a reboot), or it has not been republished for three polls (a zombie, suspended or stuck owner) or is dated more than a few seconds ahead of the clock. Receipts are scanned only while the host lock is not held. `fabric_status` and `fabric status` show available percentage, floor and elapsed/total wait, which is bounded by that attempt's `timeout_seconds`. On expiry, the attempt fails with error code `memory_unavailable` and a fix. Queued time does not count against execution timeout; cancellation remains available. The parent also excludes published queued time from its child-owner watchdog. An invalid floor records a failed attempt with a one-line fix. Admission only affects new attempts and does not stop running providers.

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

`context_ceiling` is an optional token count. Its default comes from `context.ceiling_tokens` in `config/model-routing.json` (300,000). A value outside 100,000–1,000,000 is clamped with a warning, never rejected. The ceiling only lowers a provider's compaction point; it never raises it. Codex compacts at `effective_context_window_percent` of the model's `context_window` in `$CODEX_HOME/models_cache.json` (about 258k for a 272k model). Fabric never passes `model_context_window`. Claude compacts at the model window: 1M for `opus`, `sonnet` and `fable`, 200k for `haiku`. A Haiku route remains bounded by its 200k window even if Claude answers with a larger-window model. If the user's `autoCompactWindow` in `~/.claude/settings.json` is lower, Claude compacts there instead: dispatched `--safe-mode` runs load user settings. The flag is passed only when the ceiling is below that point. When the point is unknown (a model missing from the Codex cache, no cache, or an unlisted Claude model), no flag is passed, because lowering cannot be proven. The attempt records `provider_default` with a `null` point and warns once.

The attempt records `applied.context_ceiling` as `enforced`, `provider_default` (the provider's own point is at or below the ceiling, with `applied.context_ceiling_source`) or `unsupported`. It also records `applied.context_ceiling_tokens`, the point in force (`null` when unknown), and `applied.context_ceiling_requested`. A resume inherits the prior requested ceiling unless the call passes a new one.

On macOS, Codex providers in `read-only` and `workspace-write`, and every provider under a Fabric `sandbox-exec` profile, resolve `ps` to the bundled libproc shim through PATH. The shim and `process_info.py` are staged in `<attempt>/tmp/provenant-shim`, because the product checkout may sit below a denied home or outside a read-only lane's `cwd`. The setuid `/bin/ps` cannot execute under any sandbox. The shim covers the forms agents type (`ps aux`, `ps -ef`, `ps ax`, `-p`, `-o`/`-O` with common fields) and names its supported subset when asked for more. Its `lstart` is `strftime("%c", localtime(start))` in the caller's locale, padded to 28 bytes on macOS, matching `/bin/ps` byte for byte; when libproc cannot read a live PID, `sysctl(KERN_PROC_PID)` reports its PID, parent, group, user, start and elapsed times. A missing PID remains gone. Linux retains `/proc` and normal `ps` behaviour. This PATH adjustment adds no receipt field.

A resume reads the prior attempt's context. It adds a digest warning when that context exceeds the effective ceiling (the point in force, else the requested ceiling), or when the size is unknown and the adapter has no ceiling control, for example `! resuming a ~620k-token session; fresh: fabric_dispatch{prompt, handoff:"<run id>"}`. The resume still runs. `handoff: <run id>` (with `task_id` for a batch task) is the cheaper alternative. It starts a fresh run whose prompt is prefixed with the prior task's route line and its result tail, at most 8,000 bytes in total. If the call names no adapter, alias or model, the handoff reuses the prior adapter, model and effort, and a prior writer's mode and worktree. The prior task must be terminal.

A terminal digest appends a compact marker to its Route line: `ctx 212k/1M` when observed, `ctx ~19k` when estimated, `ctx 8%` from a percentage, and nothing when unknown. The stored provenance line stays unmarked for trailers and the index. Brief status rows omit `context`; `detail: "full"` includes it.

## Receipt and provenance

Each attempt records the sorted list in `applied.capabilities`; an omitted or
empty list is `[]`.

An owner writes `fabric.attempt.v1` at `tasks/<id>/attempt-NNN/attempt.json` and aggregates it into `RUN_RECEIPT.json`. A status row is `fabric.status.v1`: the latest attempt plus attempt history, batch id where relevant, and live worktree ledger fields. The attempt records state, status, mode, cwd, workspace_root, worktree, timing, process group, session id, retry fields, evidence, question, applied sandbox/network/additional directories/confinement/guarantee/context ceiling, context, warnings, provenance, paths and digest. `workspace.cwd` is the provider's actual cwd; `workspace.root` remains the caller workspace root. Dispatch attempts, batch task rows and run receipts write `ok` for success. Readers accept `succeeded` in retained older receipts and present it as `ok`. Unknown fields may be ignored; removals bump the version.

`timing.phases` records milliseconds for available dispatch phases: TypeScript validation and catalogue snapshot, run directory setup, owner startup and attempt setup, route planning, provider spawn, provider execution and finalisation. A direct Python owner has no TypeScript phase measurements. The writer watchdog scans at most 2,000 entries per probe, checks a 25 ms budget between entries and retains its scan position; a full cycle on a large tree may take several one-second probes. Stall detection waits for a complete scan covering the idle interval so a late file is not missed.

Provenance records requested route, resolved and observed model, observation source, identity (`observed`, `resolved`, `unknown`), provider, transport, family, requested and applied effort, CLI version, fallback origin, notes and the generated line. `effort_applied` is the effort sent to the provider, clamped to the model's supported list. It is empty when none was sent: the model has no effort control (the catalogue gives it no `efforts` list, or `effort_transport: "none"`), or nothing was requested and there is no alias default. It never repeats an unsent request or guesses a provider default. The one exception is an effort the provider itself reports: with none sent, Codex's rollout `turn_context` fills it and `effort_observed_source` names that source; otherwise `effort_observed_source` is null. An ignored request stays in `effort_requested` and adds the note `effort <x> ignored: <model> has no effort control`, shown once among the digest warnings. A terminal digest and status row carry `Route: adapter/model@effort (provider; identity)`, with no `@effort` suffix when applied is empty; the retained index stores that line after run pruning. The proposed `provenant route <run-id>` index lookup is not yet wired: the current command invokes the model catalogue router. Until it is, copy the line from the digest or status row and use `Agent-Route: adapter/model@effort` for the commit trailer. Never infer a model from a worker self-report. Read-only Claude runs use `--permission-mode default` with `--tools Read,Grep,Glob`; they no longer use plan mode. Claude's observed model is the one on the assistant messages that answered (`claude:assistant.message.model`), else a lone `result.modelUsage` key, and `system.init.model` only when neither exists. Provenance keeps this comparison because Claude can still substitute under other invocations. For a substituted model, the family comes from catalogue patterns only when exactly one family matches; absent or ambiguous matches stay `unknown`, and substitutions cannot set `cross_family` or `certification_eligible`. Claude Code 2.1.280 in plan mode reports `haiku` in init but `claude-sonnet-5` (1M) answers; Opus stays Opus, and a writer's haiku answers as haiku (200k). The attempt records `init_model` and `answered_models` and warns `claude answered as <x>; init reported <y>`. If several models answered, the final top-level one goes on the Route line. Native Claude subagents use the Agent tool model parameter and record `claude/<model>@<effort> (anthropic; resolved)`.

## Layout and retention

Run root is the primary checkout root, including when dispatch starts inside a linked worktree. Runs live at `.agent-run/runs/<YYYYMMDD-HHMM>-<kind>-<slug>-<rand6>/`; owner logs live in `_owner/`. `.agent-run/sessions/` holds chair checkpoints and `.agent-run/scratch/` holds disposable files. `.worktrees/` names derive from branches with `/` replaced by `-`. New repositories exclude `.agent-run/`, `.worktrees/` and legacy `.work/` from Git locally.

`provenant clean` plans by default and prints a digest. `--apply --plan sha256:<digest>` reevaluates before deletion; clean merged worktrees can be removed without extra authority; dirty worktrees and unmerged work are never removed. Live, pinned, cited, unaccepted delivery, resumable and active mission runs are protected. A terminal dispatch attempt prunes its private `tmp/` at once (kept for `input_required`, for a failed attempt under 50 MB, or when `PROVENANT_KEEP_ATTEMPT_TMP=1`); `clean` also removes `tmp/` and `cache/` of retained attempts after one day for successful runs and seven for others, keeping receipts, results and logs for the run's retention. Sizes count allocated blocks once per inode and never follow links. Successful and cancelled runs retain seven days; typed failures retain fourteen; accepted deliveries retain thirty; owner logs retain seven; scratch retains one day. Sessions are durable handoffs and remain triage-only. Unknown paths are triage-only. If GitHub PR state cannot be read, the plan warns and holds run deletion; Git-proven merged worktrees can still expire. Worktrees require clean state and merge proof, then removal through `scripts/worktree remove`. The append-only run index keeps provenance for at least 365 days. Legacy `mcp-*`, timestamped orchestration, delivery `RUN.json` and mission `GOAL.md` readers remain for one release.

Project `CLAUDE.md` keeps both `@AGENTS.md` and `@HARNESS.md`. `scripts/install-harness` writes only a bootstrap pointer to the instance `AGENTS.md` and product `HARNESS.md` in `~/.claude/CLAUDE.md`; it does not copy their contents. Both project imports therefore provide the local rules and constitution without relying on a global file containing them.

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
special file, a replaced `.agents/` root, a tree over 256 MiB or a lane
process left running also fails. An absent `.agents/` counts as empty, since Git
removes the directory with its last file; a link or file in its place fails.
Otherwise `instruction_changes` in `<workspace_root>/.agents/fabric-policy.json`
decides. The default `quarantine` archives the lane's files as a binary patch
at `<attempt>/protected.patch`, plus `protected.index.patch` or
`protected.head.patch` when its index or HEAD held different content, each
published through the attempt directory's descriptor with an atomic replace
that never follows a planted link. It then commits the start or
integration-branch version of each path back to the lane as Provenant Fabric,
returns the index and files to their starting content (or the integration
branch's version), rechecks, and ends `ok` with a warning naming the patches;
starting files Git did not hold are stored as objects before launch so they
can be restored. `allow` keeps an ordinary edit with a warning. `deny`, a failed
quarantine, and anything quarantine cannot express (a conflict, special file,
case variant or any unverifiable state above) fail with
`protected_instructions_changed` under every policy and list the paths in the
warnings. An unknown value warns and quarantines. The check trusts local refs, which the lane can
move. It also reports a clean three-way merge into a skill the branch already
changed, a refresh overtaken by a newer integration-branch change to the same
file, a refresh under line-ending conversion, and any refresh in a repository
that tracks a case variant such as `.Agents/`. Applying the saved patch from
the primary checkout lands such a refresh, and a retry clears an overtaken one. Review of the branch diff
remains the backstop.

`capabilities` is an optional list with distinct values from `postgres` and
`browser`; an empty list means absent. Non-Codex adapters apply it without a
grant, because their `sandbox-exec` profiles restrict only file access; a
browser lane sets `MAC_CHROMIUM_TMPDIR` to `<attempt>/tmp`. A Codex
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
registration prefixes; browser lanes set `MAC_CHROMIUM_TMPDIR` to
`<attempt>/tmp` for Chrome's process-singleton socket. These capabilities add no
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
`TMP` and `TEMP` to `<attempt>/tmp`, `XDG_CACHE_HOME` to `<attempt>/tmp/cache` and
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

`model_route.py snapshot --json` is the single merged catalogue source. Unknown model IDs pass through with a note when runnable; unsupported effort substitutes to the nearest supported value. Explicit cooling models run with a warning. A hard rejection is reserved for impossible execution or a hard boundary. Per-run flags are preferred; editing global provider configuration requires explicit authority. Credentials never appear in argv, receipts or logs. Provider guarantees are reported as `enforced`, `best_effort` or `prompt_only` according to observed controls. On macOS, read-only agy and OpenCode launches use `sandbox-exec` when available to deny workspace reads outside `cwd` and `add_dirs`, and deny workspace writes. Every writer, Codex included, has one Git write boundary: its per-worktree Git directory (`--absolute-git-dir`) plus the common directory's `objects`, `refs`, `logs`, `packed-refs`, `packed-refs.lock` and `packed-refs.new`; the rest of the common directory (`hooks`, `config`, `info`, other lanes' `worktrees/<name>`) is never writable, and a Codex writer drops an `add_dir` at or inside it with a warning. Codex writers also keep the worktree `.git` marker and the private directory's `config.worktree`, `commondir` and `gitdir` read-only; Codex refuses to launch with a symlinked grant path. Writer attempts set `gc.auto=0`, `maintenance.auto=false` and `rerere.enabled=false` through `GIT_CONFIG_COUNT`. Because the shared `config` is read-only, a lane pushes with `git push origin HEAD` and opens its pull request with `gh pr create --head <branch>`; `git push -u` pushes but cannot record the upstream. Wrapped writer launches (agy, Claude, Cursor, OpenCode and Kiro) restrict writes to the worktree, declared `add_dirs`, the Git write boundary, attempt files, temp paths, devices and provider state. Where protected-path policy applies, its read and write denies take precedence within an `add_dir`; receipts list the writable `add_dirs` in `applied.write_boundary`. Codex writers without capabilities use its native sandbox with that permissions profile, recorded as `applied.write_boundary.filesystem`. Unavailable OS confinement refuses agy write dispatches and produces an explicit warning for other wrapped writers.
