# Fabric

Fabric provides a project mailbox, task ledger and a thin MCP front door to the
Python orchestration owners. Full provider output stays in run files.
One SQLite file, no daemon, no setup.

## Start

```sh
provenant fabric whoami
runtime/fabric/bin/fabric-mcp
```

`bin/fabric` and `bin/fabric-mcp` run TypeScript directly with `tsx`;
there is no build to keep in step with the source. Node 24.15 or newer within
major version 24 is required. `FABRIC_NODE`, `AGENT_FABRIC_TSX_LOADER` and
`AGENT_FABRIC_PRODUCT_ROOT` select the runtime, loader and product checkout.
Restart an existing MCP connection after changing source. `fabric_whoami`
reports `server_version`, `build_stale` and a restart fix when needed.
`install-harness --platform all` registers Fabric for detected client homes;
each client has its own seat and inbox, and a detected client with conflicting
instructions is skipped and reported. `check-provenant-install.py` warns about
routing drift (`--strict` fails); `install-harness` refreshes the instance
catalogue whenever a product snapshot exists, preserving three-way instance
overrides. Without a snapshot it skips and prints a hint; `--refresh-routing`
forces a refresh.

## MCP quickstart

```text
fabric_dispatch{prompt:"Review the change",adapter:"codex",model:"luna",effort:"high"}
fabric_status{ids:["mcp-a81f3c"],wait_seconds:55}
fabric_output{id:"mcp-a81f3c",part:"result",max_bytes:4000}
```

Copy the returned `Route:` line for provenance. `content` contains the owner's
verbatim digest; `structuredContent` contains minimal `fabric.status.v1` rows by
default. `detail: full` includes attempt history, evidence and provenance. A small
formatter supports older receipts when no digest exists. Request errors contain
one line with `fix:`. Recognised field names and mode synonyms are corrected;
model names accept case and punctuation variants, and a typo is corrected with a
warning when exactly one name is close and its version numbers match. Anything
else is rejected with the closest valid choices. Relative `cwd` and `worktree` paths resolve from
the caller directory.
No provider output is embedded in status responses.

Without an MCP connection, `provenant fabric dispatch --adapter A --model M
--effort E --mode read_only|worktree_write --prompt-file F [--wait]` uses the
same dispatcher. Add `--worktree P`, `--cwd P` or `--id ID` as needed. Use
`--tasks F` for a JSON object with `tasks[]` and shared route fields. It prints
the Fabric run id and status; detached owners remain discoverable after the CLI
or MCP host exits.
Batch owners are detached session leaders and survive MCP-host restart; a fresh
`dispatch list` and `status <run-dir>` resolve them from their run records.

Fourteen tools are registered by default:

| Tool | Purpose |
| --- | --- |
| `fabric_dispatch` | One prompt, `tasks[]`, `resume` or `handoff` |
| `fabric_status` | Read run/task/batch IDs; bounded wait for `any` or `all` |
| `fabric_runs` | Versioned run and lane list with root-relative paths |
| `fabric_events` | Cursor-based terminal, input-required and inbox events |
| `fabric_cancel` | Cancel the owner and provider group |
| `fabric_output` | Bounded result, stderr, events or receipt slice |
| `fabric_adapters` | Compact catalogue, CLI availability and guarantees |
| `fabric_whoami` | Seat, project and server freshness |
| `fabric_send` | Send to a seat, team, chair or all |
| `fabric_inbox` | Peek headers or claim selected messages |
| `fabric_acknowledge` | Acknowledge using the claim token |
| `fabric_note` | Append activity |
| `fabric_activity` | Read recent activity or continue after a cursor |
| `fabric_task` | `action: create`, `claim`, `update` or `list` |

`FABRIC_LEGACY_TOOLS=1` additionally registers `fabric_batch`,
`fabric_team_create`, `fabric_task_create`, `fabric_task_claim`,
`fabric_task_update` and `fabric_tasks`. Teams remain available through those
legacy tools; the default task interface is `fabric_task`.

Dispatch accepts exactly one of `prompt` and `prompt_file`. Route controls are
`adapter`, `alias`, `model`, `effort`, `mode`, `worktree`, `cwd`, `network`,
`sandbox`, `add_dirs`, `fallback` and `allow_secrets`. Before dispatch, Fabric scans
the prompt and eligible files under `add_dirs` for common live credential shapes.
A finding rejects with `error: secret_detected` and a location in `fix`; set
`allow_secrets: true` explicitly to proceed. The attempt records the override
and finding names. If scanning `add_dirs` exceeds 2,000 files or 20 MB, dispatch
rejects with `error: secret_scan_budget_exceeded`; narrow the inputs or explicitly
set `allow_secrets: true` and explain why in the prompt. Writers use `mode: worktree_write` and an
owned, registered linked worktree. The primary checkout is refused; create a linked
worktree. `cwd` selects an existing read-only directory inside any registered
Fabric project. The Python owner validates provider capabilities and
applies controls; Fabric does not claim a stronger guarantee than its receipt.
On macOS, non-Codex read-only launches use `sandbox-exec` when available. The
profile limits writes to the attempt directory and provider state. It denies
reads of home, shared temp and the workspace outside `cwd` and `add_dirs`, with
provider sign-in paths re-allowed. Training routes also deny reads of protected
paths. Codex read-only uses its native read-only sandbox.
Inside another sandbox, where macOS refuses a nested one, the attempt records
an explicit unconfined-write warning. A `cwd` below the root may also warn that
it is not a read boundary.
Wrapped writer runs on macOS (agy, Claude, Cursor, OpenCode and Kiro) use
`sandbox-exec` to restrict writes to their worktree, declared `add_dirs`,
per-worktree Git metadata, common Git objects, refs, logs and packed refs,
attempt files, device nodes and provider state. Where protected-path policy
applies, its read and write denies still take precedence inside an `add_dir`.
Each attempt sets `TMPDIR`, `TMP`, `TEMP` and `XDG_CACHE_HOME` to private `tmp`
and `cache` directories under its run directory. Shared temp and general user
caches are not writable. Codex
uses `-s workspace-write` (or `-c sandbox_mode="workspace-write"`
on resume), with `-c sandbox_workspace_write.writable_roots=<add_dirs>` and
`--cd <worktree>` on a fresh run. If OS confinement is unavailable, agy write
dispatch is refused; other wrapped writer receipts warn that writes are
unconfined. Setting `PROVENANT_NO_OS_CONFINEMENT=1` has the same effect on new
wrapped attempts.
Protected-path dispatches to training routes still refuse without OS read
confinement.
The receipt records `applied.confinement` as `sandbox-exec`, `provider-native` or `none`;
`applied.write_boundary` records the effective writable paths, including
declared `add_dirs` for confined writers, or the native sandbox;
`workspace.cwd` is the provider cwd and `workspace.root` is the caller workspace.
Read-only macOS launches can read `~/.gitconfig` and
`$XDG_CONFIG_HOME/git/config` (default `~/.config/git/config`) so `git status`
works. Other home-directory reads remain denied apart from provider state and
sign-in files listed by the adapter profile.
Projects declare protected paths in `.agents/fabric-policy.json`, relative to
the directory holding `.agents/`. Fabric checks the workspace root and the Git
toplevels of the workspace, cwd and worktree; for a non-Git workspace, it also
checks immediate child repository toplevels and mirrors repository paths into
their registered worktrees. A route marked as training, or lacking a resolved
training flag, cannot receive a prompt file or additional directory that
overlaps one, or a cwd inside one. Its sandbox denies reads of protected paths
in every registered worktree; without usable `sandbox-exec`, dispatch is rejected. Non-training
routes keep their usual access.

On macOS, a Codex `read-only` or `workspace-write` provider gets a bundled `ps`
shim on PATH because seatbelt blocks the setuid `/bin/ps`. Process identity reads
use libproc and preserve the recorded C-locale start time. Fabric falls back to
the PATH command when `/bin/ps` cannot execute; unreadable PIDs stay unverifiable.

`tasks` contains 1–64 task objects with the same prompt and route fields plus
optional `id`; `concurrency` is 1–8. Top-level route controls and timeout apply
as defaults, with each task taking precedence. Prompt file paths resolve from
the caller workspace, including when `cwd` selects a subdirectory. Defaults are
55 seconds of waiting for a single dispatch and zero for a batch. Timeouts default to 3,600 seconds for
read-only work and 10,800 seconds for writers. `resume` retains the same run ID,
route and controls; only `context_ceiling`, `timeout_seconds`, and `allow_secrets` may change. Use a new
dispatch to change the rest. With `resume`, `task_id` selects one task of a
batch; a task's own ID also works. `handoff: <run id>` starts a fresh run primed
with that task's route and result tail, the cheap alternative to resuming a large
session. `context_ceiling` (default 300,000 tokens, clamped to 100k–1M) lowers
auto-compaction where the provider supports it; it never raises it. Each attempt records `context`,
and the terminal Route line shows it as `ctx 212k/1M`. See
[`docs/specs/fabric-v2.md`](../../docs/specs/fabric-v2.md#session-context).

Status accepts `ids`, `wait_seconds` (0–55), `until: any|all`, and `detail`.
New attempts wait when available host memory is below 10% of physical RAM for
`worktree_write` or 5% for `read_only`; set either percentage from 0 to 100 in
`<workspace_root>/.agents/fabric-policy.json` as `{"memory_floor_percent":{"worktree_write":10,"read_only":5}}` (either key may be omitted, and 0 disables that mode's floor).
Queued time does not use the execution timeout, but `FABRIC_MEMORY_WAIT_SECONDS`
limits each wait (default 1800); expiry fails the attempt as `memory_unavailable`.
Owners serialise admission through a per-user host lock in `$XDG_STATE_HOME/provenant/admission.lock`
(default `~/.local/state/provenant/admission.lock`) and hold it for up to 20 seconds after provider start; lock creation failure admits with a warning, while probe failure holds and retries until the wait expires. `fabric_status` and `fabric status` show the available percentage, floor and wait budget and allow cancellation.

### Live provider sandbox smoke

The live check is opt-in and makes real provider calls. On macOS with
`sandbox-exec`, run:

```sh
python3 runtime/fabric/live-sandbox-smoke.py --execute
```

It dispatches one short `read_only` and one `worktree_write` task for each
installed writer adapter among Codex, Claude, Kiro and OpenCode. Each task
probes a declared protected file and a write target outside its boundary. The
command exits nonzero if a probe fails and prints JSON containing each adapter,
mode, resolved `Route:` line, confinement, warnings and result. It creates its
Git fixtures and run artifacts in a temporary directory, which is removed when
the command exits. Do not add this command to the default test run. Record the
first run's JSON results in the pull request.
An invalid floor records a failed attempt with a fix. The parent keeps a
watchdog for child owners and excludes their published queued time.

The wave-1 `id` argument and retained `mcp-*` directories remain readable.
Without IDs it returns active and last-24-hour runs, capped at 20 rows. Rows
include the latest attempt and count. Full detail adds history and the worktree
ledger: branch tip, dirty state and ahead count; unavailable Git facts are null.
Ledger reads are shared per worktree within a response and omitted for terminal
brief rows. New successful attempt, batch task and run statuses are `ok`.
Status and output readers accept `succeeded` in older retained files.
Unpublished batch children remain visible until an attempt or terminal batch
summary accounts for them.

`provenant lanes --json` and `fabric_runs` expose `fabric.runs.v1`. The response
has `status: ok|unknown` and `runs`; read failure is `unknown` with an `error`,
never an apparently empty successful list. The default list covers active and
recent runs (at most 20 task rows); an ID reads that run or task. Each row has
`id`, `run_id`, `task_id`, `run_path`, `state`, `status`, `route`, `model`,
`started_at`, `last_progress_at`, `pgid`, `pgid_alive`, `result_path`,
`receipt_path`, `writer`, `worktree` and `attempt`. Paths are relative to the
project's `.agent-run` root. Missing facts are `null`; `pgid_alive: null` means
the process identity cannot be verified. Both `tasks/<id>/attempt-NNN/` and
`dispatch/tasks/<id>/attempt-NNN/` receipts are read internally. Fabric's status
and output readers use the same underlying receipt scanner; consumers should
use the versioned response instead of opening receipt files themselves. The
response reserves an optional `claims` field for the work-claims reader.

`provenant events --follow` prints one JSON line per new terminal or
`input_required` attempt and per unread inbox message, and stays open until
stopped. Add `--until-idle` to exit once no run is active and a poll finds no
new events; this lets a background monitor wake a chair and finish. `fabric_events` returns
`fabric.events.v1` with `events` and an opaque `cursor`; pass that cursor back
with `wait_seconds` (0–55) to wait for changes. Run one follower per session;
restart it only after exit. The stream is a bounded recent view of retained
attempts and the active inbox, so a chair should reconcile with `fabric_runs`
after a long disconnect. Arm a fallback wake of at least 20 minutes.

Output defaults to 4,000 bytes and caps each request at 20,000. Continue at
`next_offset`; pages preserve UTF-8 boundaries and `eof` reflects the current
file size. Use `tail: true` for a bounded tail, including while a run is active.
Paths must resolve to regular files inside the retained run directory. For a batch, select a task ID.
`detail: full` adds adapter profiles or the agent list to discovery responses.
CLI presence does not prove authentication; `auth?` makes that uncertainty explicit.

## Mailbox and identity

`fabric_inbox` defaults to a non-claiming peek of ten headers: ID, sender, kind
and an 80-character preview. `ids:[...]` claims up to 100 selected messages;
`claim:true` claims available messages up to `limit`. Bodies are capped at
4 KiB each, with `body_path` for the full text. Deliveries older than fourteen
days stay in storage but are excluded from the active inbox.

`fabric_inbox{digest:true}` and `fabric inbox --digest` return
`fabric.inbox_digest.v1`: unread `total`, up to 20 `groups` by sender and task,
and `truncated`. Each group has a count, an 80-character one-line summary and a
sample message ID. Fetch a full body by calling `fabric_inbox` with `ids:[id]`;
the digest neither claims nor acknowledges deliveries.

A claim lasts five minutes by default. Acknowledge only after processing the
message; expired claims redeliver. Claim tokens prevent another reader from
acknowledging the delivery. `wait_seconds`, `task_id`, `peek` and
`claim_seconds` remain available for existing integrations.

`chair` resolves through `PROVENANT_CHAIR`; `/root`, `root` and `parent` use
`PROVENANT_PARENT`, then the chair. Only known project seats are selected. An
unbound caller can use a registered seat named `chair`; otherwise the request
is rejected with `fix: pass to:<seat>`. Fabric never guesses the first seat.
Owner completion posts `run_terminal` to the dispatching seat. Observing a
terminal row through dispatch, cancel or status acknowledges its notices,
including a notice that arrives after the response.

Registered Git worktrees share one project while retaining their own cwd.
`AGENT_FABRIC_LABEL` separates seats of one provider. A label remains bound to its
first announced provider. Task ownership is cooperative routing metadata, not
an access-control boundary. Reply parents and linked task IDs must exist in the
same project.

The shell mailbox retains its explicit claim workflow:

```sh
fabric inbox --peek
fabric inbox --limit 5
fabric ack <message-id> <claim-id>
fabric watch mcp-a81f3c mcp-b22c       # state changes; exits when all terminal
fabric watch --activity --interval 2 # activity stream
fabric status <run-id> --wait-seconds 55
fabric adapters --json
```

### Work claims and landing

`fabric_work_claim` records an advisory issue or repository-relative path claim
for one chair session. Conflicting live issue claims and overlapping path
prefixes are refused on acquisition; dispatch does not enforce claims. Acquire
and verify a claim before dispatch or landing, and do not dispatch against a
conflicting live claim. Pass a distinct, stable `session_id` without `/` for
each chair, even when two sessions use the same Fabric seat. The returned `id`
and `generation` identify renew, verify and release operations; expired claims
lapse. `#869` and `869` share an issue key, and path claims compare without case.
Active claims appear in `fabric work-claims` and `fabric_status` when
full detail is requested or ownership is present.

`fabric_landing_lease` separately acquires one repository-wide landing lease
with the expected remote integration SHA. It has a holder, expiry, generation,
verify and release operations. An expired lease can be taken over; the prior
holder and new generation are recorded in activity. Before pushing, run
`fabric landing-push <session-id> <generation> <branch> --label <seat>` from the
landing checkout, using the same label as the MCP lease holder. Alternatively,
export that label as `AGENT_FABRIC_LABEL` in the shell. The command checks the
remote SHA and live lease, confirms the remote commit is an ancestor of HEAD,
then pushes with `--force-with-lease` and releases the lease on success. The
command waits at most two minutes for Git; its persisted hold lasts another
thirty seconds. A push orphaned by a crash is fenced by the remote SHA, not by
the hold. If the push succeeds but release fails, it reports `pushed` with a
`release_warning`. Renew or release an aborted lease as appropriate.

## Run storage and boundaries

`identity.runRoot(cwd)` uses the primary checkout's `.agent-run` for Git and the
workspace directory's `.agent-run` otherwise. New runs are stored under
`runs/YYYYMMDD-HHMM-dispatch|batch-slug-rand6/`; IDs stay `mcp-rand6`.
Owner stdout, stderr and staging inputs live inside `_owner/`. New run creation
adds `/.agent-run/`, `/.worktrees/` and `/.work/` to Git's local exclude file when
writable. Existing `mcp-*` paths remain readable in both the shared root and
the caller worktree’s former local `.agent-run` root.

Dispatch-time maintenance scans the shared root and retained local legacy runs.
It closes active receipts
older than 48 hours with no observed live owner as `interrupted`. Retention is
seven days for successful/cancelled runs and fourteen days for failures
(`AGENT_FABRIC_RUN_RETENTION_HOURS` overrides). Live runs,
`KEEP`, active receipts and delivery `RUN.json` files are preserved; unknown new
run directories are left for the cleanup owner. PID start identity is required
before signalling a recorded process group. A missing `ps` capability cannot
certify process death or safe cancellation.

`AGENT_FABRIC_STATE_DIRECTORY` defaults to
`~/.local/state/agent-harness/fabric`. New directories use mode 0700; existing
permissions are preserved. SQLite WAL, immediate claims and durable terminal
observation records handle concurrent readers. The boundary is one local OS
user; the mailbox is not a security boundary between processes of that user.

## Verification and integration

```sh
npm --prefix runtime/fabric run typecheck
npm --prefix runtime/fabric test
node runtime/fabric/mcp-smoke.mjs
node runtime/fabric/tests/tool-budget.mjs
```

The MCP smoke uses fixture owners through the registered launcher. It covers
a real linked worktree, writer arguments, inherited Git redirects, question/resume,
failed-resume recovery, status, cancel, bounded output, two-seat reply correlation,
owner-bound tasks, mailbox claims and terminal-notice acknowledgement.
The layout test reads the shared `tests/fixtures/fabric-v1/layout-cases.json`,
including its symlinked-cwd case.

Before: 15 tools, 9,077 characters, approximately 2,269 tokens (`chars / 4`).
The default v2 tool-list test enforces at least a 35% reduction.
