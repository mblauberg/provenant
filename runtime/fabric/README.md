# Fabric

Fabric provides a project mailbox, task ledger and a thin MCP front door to the
Python orchestration owners. Full provider output stays in run files.

## Start

```sh
provenant fabric whoami
runtime/fabric/bin/fabric-mcp
```

The launchers run TypeScript directly with `tsx`; Node 24.15 or newer within
major version 24 is required. `FABRIC_NODE`, `AGENT_FABRIC_TSX_LOADER` and
`AGENT_FABRIC_PRODUCT_ROOT` select the runtime, loader and product checkout.
Restart an existing MCP connection after changing source. `fabric_whoami`
reports `server_version`, `build_stale` and a restart fix when needed.
`install-harness --platform all` registers Fabric for detected client homes;
each client has its own seat and inbox, and a detected client with conflicting
instructions is skipped and reported. `check-provenant-install.py` warns about
routing drift (`--strict` fails); `install-harness --refresh-routing` merges
product catalogue changes into the instance copy and lists conflicts.

## MCP quickstart

```text
fabric_dispatch{prompt:"Review the change",adapter:"codex",model:"luna",effort:"high"}
fabric_status{ids:["mcp-a81f3c"],wait_seconds:55}
fabric_output{id:"mcp-a81f3c",part:"result",max_bytes:4000}
```

Copy the returned `Route:` line for provenance. `content` contains the owner's
verbatim digest; `structuredContent` contains `fabric.status.v1` rows. A small
formatter supports older receipts when no digest exists. Request errors contain
one line with `fix:`. No provider output is embedded in status responses.

Exactly twelve tools are registered by default:

| Tool | Purpose |
| --- | --- |
| `fabric_dispatch` | One prompt, `tasks[]`, or `resume` |
| `fabric_status` | Read run/task/batch IDs; bounded wait for `any` or `all` |
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
`sandbox`, `add_dirs` and `fallback`. Writers use `mode: worktree_write` and an
owned, registered worktree. `cwd` selects an existing read-only directory inside
the caller workspace. The Python owner validates provider capabilities and
applies controls; Fabric does not claim a stronger guarantee than its receipt.

`tasks` contains 1–64 task objects with the same prompt and route fields plus
optional `id`; `concurrency` is 1–8. Defaults are 55 seconds of waiting for a
single dispatch and zero for a batch. Timeouts default to 3,600 seconds for
read-only work and 10,800 seconds for writers. `resume` retains the same run ID,
route, controls and timeout. Use a new dispatch to change those settings.

Status accepts `ids`, `wait_seconds` (0–55), `until: any|all`, and `detail`.
The wave-1 `id` argument and retained `mcp-*` directories remain readable.
Without IDs it returns active and last-24-hour runs, capped at 20 rows. Rows
include the latest attempt, attempt history and count, worktree, branch tip, dirty state and
ahead count; unavailable Git facts are null. Unpublished batch children remain
visible until an attempt or terminal batch summary accounts for them.

Output defaults to 4,000 bytes and caps each request at 20,000. Continue at
`next_offset`; `eof` reflects the current file size. Paths must resolve to
regular files inside the retained run directory. For a batch, select a task ID.
`detail: full` adds adapter profiles or the agent list to discovery responses.
CLI presence does not prove authentication; `auth?` makes that uncertainty explicit.

## Mailbox and identity

`fabric_inbox` defaults to a non-claiming peek of ten headers: ID, sender, kind
and an 80-character preview. `ids:[...]` claims up to 100 selected messages;
`claim:true` claims available messages up to `limit`. Bodies are capped at
4 KiB each, with `body_path` for the full text. Deliveries older than fourteen
days stay in storage but are excluded from the active inbox.

A claim lasts five minutes by default. Acknowledge only after processing the
message; expired claims redeliver. Claim tokens prevent another reader from
acknowledging the delivery. `wait_seconds`, `task_id`, `peek` and
`claim_seconds` remain available for existing integrations.

`chair` resolves through `PROVENANT_CHAIR`; `/root`, `root` and `parent` use
`PROVENANT_PARENT`, then the chair. Only known project seats are selected. An
unbound caller falls back to the named chair or the first registered seat.
Owner completion posts `run_terminal` to the dispatching seat. Status observation
acknowledges notices through the returned terminal attempt, including a notice
that arrives after the status response.

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

## Run storage and boundaries

`identity.runRoot(cwd)` uses the primary checkout's `.agent-run` for Git and the
workspace directory's `.agent-run` otherwise. New runs are stored under
`runs/YYYYMMDD-HHMM-dispatch|batch-slug-rand6/`; IDs stay `mcp-rand6`.
Owner stdout, stderr and staging inputs live inside `_owner/`. New run creation
adds `/.agent-run/`, `/.worktrees/` and `/.work/` to Git's local exclude file when
writable. Existing `mcp-*` paths remain readable.

Dispatch-time maintenance scans that shared root. It closes active receipts
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

The MCP smoke uses fixture owners only, including a real linked worktree,
question/resume, duplicate-resume rejection, failed-resume recovery, status,
cancel, bounded output, mailbox claims and terminal-notice acknowledgement.
The layout test prefers `tests/fixtures/fabric-v1/layout-cases.json`; its local
fallback is an unchanged copy from Lane A, with a separate symlinked-cwd test.

Before: 15 tools, 9,077 characters, approximately 2,269 tokens (`chars / 4`).
The default v2 tool-list test enforces at least a 35% reduction. Verification
counts and remaining integration dependencies are recorded in
[the Lane B evidence](tests/fixtures/lane-b-verification.json).
