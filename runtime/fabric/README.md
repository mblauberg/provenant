# Fabric

Fabric is a project-scoped mailbox, small shared task ledger and activity log.
One SQLite file, no daemon, no setup. Each process opens it directly. Its MCP
surface also provides a thin front door to the existing dispatch and batch
owners; Fabric has no provider implementation, scheduler or workflow engine.

## Start

The managed `provenant` shim is the stable entry point. The package launchers
also run the TypeScript in place, so there is no build to keep in step with the source:

```sh
provenant fabric whoami
runtime/fabric/bin/fabric whoami
runtime/fabric/bin/fabric-mcp
```

The package carries `tsx` as a runtime dependency. The launchers resolve it
from an installed package or the product root.
`AGENT_FABRIC_PRODUCT_ROOT` selects an installed product checkout and
`AGENT_FABRIC_TSX_LOADER` can name an explicit loader. `FABRIC_NODE` can name
the Node binary. Launchers require Node `>=24.15.0` and `<25`.

Common CLI operations are:

```sh
fabric send codex "review auth.ts" --kind request
fabric send codex "result is ready" --task-id review-auth --output-path /tmp/review.md
fabric inbox                         # claim available deliveries (default: 20)
fabric inbox --limit 5               # claim at most five deliveries
fabric inbox --task-id review-auth    # claim only that task's deliveries
fabric ack <message-id> <claim-id>   # acknowledge after receipt succeeds
fabric task "review the change"
fabric claim <task-id>
fabric tasks
fabric watch --interval 2
fabric status --json                 # read-only; absent state is valid
fabric doctor --json                 # read-only schema and integrity checks
```

Run `fabric --help` for every argument. Unknown commands are rejected before
the state directory is opened or an agent is announced. Expected CLI failures
are concise and do not print Node stack traces.

## Delivery contract

`fabric inbox` atomically claims each returned delivery. The response contains
`claimId` and `claimExpiresAt`. Another process sharing the same label cannot
receive that active claim. A successful transport is not an acknowledgement:
the consumer must call `fabric ack` or `fabric_acknowledge` after it has the
message.

The default claim lifetime is five minutes. CLI and MCP callers may choose from
one second to one hour. An unacknowledged expired claim is available for
redelivery on the next inbox call; no background process is required. A stale
claim token cannot acknowledge a delivery after another reader reclaims it.
Repeating a successful acknowledgement with the same token is idempotent.

MCP callers may set `wait_seconds` from zero to 55 on `fabric_inbox`. An empty
inbox then waits inside that one tool call until a message arrives or the bound
expires. It returns `[]` on expiry and stops without claiming a later message
when the caller cancels. Do not query Fabric's SQLite file or start a shell
watcher; return or make another bounded MCP call instead.

`inbox --peek` is observation-only: it neither claims nor acknowledges. It can
show an actively claimed delivery, but never reveals that reader's claim token.
Existing databases keep their rows. A legacy non-null `read_at` remains an
acknowledged delivery; the additive `delivery_claims` table holds new claims.

A supplied reply parent must already exist in the caller's project. Missing,
stale and cross-project IDs fail before a message or activity row is inserted.
Messages may also carry an existing same-project `task_id` and an opaque
`output_path`. The path is metadata only: Fabric never reads, stats, hashes,
canonicalises or
grants authority to it. Replies do not inherit either link unless supplied.
`fabric_inbox` can filter by `task_id`; filtering happens before any claim, so
unrelated pending deliveries remain available and redelivery is unchanged.

## Identity and scope

An identity is `(project, agent_id)`:

- `project` is the primary checkout shared by ordinary registered Git
  worktrees, otherwise the Git top level or absolute directory outside Git;
- `cwd` is the caller's resolved working directory;
- `agent_id` is `AGENT_FABRIC_LABEL`, falling back to the client seat; and
- the client seat is `AGENT_FABRIC_SEAT`, then
  `AGENT_FABRIC_CLIENT_LABEL`, then `agent`.

The first announcement binds one project/label to one client seat. Reusing that
label under another seat is rejected. New team IDs cannot reuse a known agent
label. A legacy database may already contain an overlapping team and agent ID;
startup remains compatible, team routing wins deterministically, and `doctor`
reports the ambiguity so it can be retired deliberately.
The recipient ID `all` is reserved for broadcast and cannot be announced as an
agent, created as a team, or used as a team member.

A seat is routing metadata, not model-family proof. In particular,
`provider: "agy"` proves only that the Agy client used that seat. A separate
dispatch receipt must establish whether Agy selected a Gemini-family or other
model route.

Several processes may deliberately share one label. They then compete for the
same inbox claims, while a distinct `AGENT_FABRIC_LABEL` gives each process its
own address.

Existing rows keyed by an ordinary primary checkout remain valid. Rows
previously written under a linked-worktree path are left untouched; Fabric does
not guess at or bulk-rewrite old coordination state.

## Teams, tasks and activity

`fabric_team_create` creates a team or atomically replaces all membership of an
existing team. Its returned member list is the effective stored set. This
replacement contract avoids silent delivery to members omitted from the latest
call.

Tasks keep their existing free-form state. `fabric_task_claim` and CLI `claim`
add only one concurrency rule: exactly one caller can take an `open`, unowned
task. Retrying as that owner is idempotent; other callers fail. Generic task
updates remain cooperative and do not form a state machine, except that the
literal state `claimed` is reserved for the atomic ownership operation.
Create targeted tasks with the MCP `owner` field. An owner-bound task is already
assigned and is not available to unowned-task claiming; retrying as that owner
is idempotent. Task ownership is cooperative routing metadata, not an
access-control boundary, and does not grant or restrict tool or filesystem
access.

Fabric derives identity from the process working directory. The primary checkout
and all of its registered linked worktrees share messages, tasks, teams and
activity without configuration; `cwd` still shows where each caller is working.
Separate repositories, copied worktree metadata and non-Git directories remain
separate projects.

Git does not record a main working-tree path for separate-git-dir, bare-main or
submodule layouts. Fabric keeps those ambiguous working trees separate instead
of guessing an alias that a copied checkout could inherit.

Activity entries expose their monotonic `seq`. `fabric_activity` accepts
`after_seq` for ascending cursor reads. CLI `watch` uses that cursor and drains
bounded pages, so it continues after the first 200 rows and across larger
bursts.

## MCP surface

The MCP server announces its identity at ordinary startup and exposes:

```text
fabric_whoami       fabric_send          fabric_inbox
fabric_acknowledge  fabric_team_create   fabric_task_create
fabric_task_claim   fabric_task_update   fabric_tasks
fabric_note         fabric_activity      fabric_dispatch
fabric_status
fabric_batch        fabric_adapters
```

### Happy path

Discover configured adapters once with `fabric_adapters` (CLI: `fabric adapters`).
It lists aliases, concrete models and read-only guarantees from the instance
`$AGENT_FABRIC_INSTANCE_ROOT/config/model-routing.json` (default `~/.agents`),
using the product catalogue only when the instance file is absent.

```json
{ "prompt": "review auth.ts", "adapter": "codex", "model": "gpt-6-luna" }
```

`fabric_dispatch` takes `prompt` or a readable `prompt_file` inside the workspace,
plus an adapter (default: the current provider seat). Optional `model` selects
an explicit id, including broker ids such as `opencode/<id>`; optional `alias`
selects `flagship`, `workhorse` (default), or `scout`. A unique catalogue model
name such as `luna`, `sol`, `astra` or `opus` also works in `alias`. Pass one
selector and, optionally, `effort`: `low`, `medium`, `high`, `xhigh`, `max` or
`ultra`. The router still enforces model and effort admissibility.

Mode defaults to `read_only`; inspect the adapter's `read_only_guarantee`
(Agy is `prompt_only`). Writers pass `mode: "worktree_write"` and the registered
Git `worktree` they own exclusively. Default timeouts are 3600 seconds for
reads and 10800 for writes; `timeout_seconds` overrides them.

`fabric_batch` takes 1–64 tasks with the same fields and optional concurrency
(up to eight). Every task is checked before anything launches, with one
capability probe per adapter per call. Bad inputs return `status: "rejected"`,
an `error` code and a one-line `fix`; batches include per-task errors. Rejection
creates no run directory, and two writer tasks cannot share a worktree.

Both tools retain full output in files and return compact status and paths.
`wait_seconds` defaults to 55; zero returns after preflight and launch. Continue
with `fabric_status({id: task_id_or_batch_id_or_run_dir, wait_seconds: 55})`.
Omit `id` for at most 20 workspace runs from the last 24 hours. Reused task or
batch identifiers require the returned `run_dir` to disambiguate.
The CLI equivalents are `fabric status <id> --wait-seconds 55` and
`fabric status --runs`; bare `fabric status` retains the store summary.

Status reads owner/provider liveness and retained stdout/stderr/result mtimes without
writing to SQLite or starting background work. It reports elapsed time,
seconds since output, and `stalled: true` when a live run has been silent longer
than the greater of 600 seconds or 20% of its timeout. A dead owner without a
terminal record is `interrupted`. Timeout and cancellation records retain route
metadata, including the preflight route when the provider emitted no receipt.

Dispatch and batch owners retain their chair context, but provider processes
start without the chair's Fabric state directory, seat, client label, agent
label or product-root override. Worker tests and commands discover their own
workspace instead of using the chair's state or checkout.

MCP execution owners close `RUN_RECEIPT.json` after all attempts finish. This is
a minimal execution-status update under the existing custody lock: the delivery
finaliser requires synthesis and review gates that ordinary provider tasks do
not have. It does not certify delivery acceptance, and manual orchestration runs
keep their existing finalisation workflow.

Run retention is dispatch-time-only: starting a dispatch prunes that
workspace's `.agent-run/mcp-*` runs older than the retention window (default
168 hours, `AGENT_FABRIC_RUN_RETENTION_HOURS` overrides, `0` keeps nothing);
workspaces that only ever read never prune. `fabric dispatch list` reports the
effective `retention_hours` for the workspace. There is no background reaper
and no `run gc` command by design — retention is a bounded side effect of the
next dispatch, not a second lifecycle.

The existing run controls inspect, retry or cancel an execution after the MCP
call returns. Closing the MCP transport asks any owner started by that process
to terminate. Fabric does not add a session database, transcript copy,
scheduler, retry policy, model-family gate or delivery receipt.

`mcp-smoke.mjs` asserts this wire contract with Claude, Codex and Agy client
seats. Set `AGENT_FABRIC_MCP_COMMAND` to the managed `provenant` shim to test
stable installed routing. The smoke forwards an explicit product root and
loader, so a branch run cannot silently certify another checkout.
`npm run test:package-install` packs and installs the package in a temporary
prefix, then asserts its installed CLI and MCP bins without a product checkout
or loader override.

## State and security boundary

`AGENT_FABRIC_STATE_DIRECTORY` selects the state directory. The default is
`~/.local/state/agent-harness/fabric`. Newly created directories use mode
`0700`. Fabric does not change an existing directory's permissions because the
caller may have supplied a shared parent; inspect and correct that directory
before use.

The threat model is one local operating-system user. Every participating
process can open the same file, so capability tokens would not create a real
isolation boundary. SQLite WAL and immediate transactions provide the required
concurrency. Provider lifecycle and cancellation remain with the existing
orchestration owners; Fabric only forwards transport closure to a child it
started. Wake-up remains outside Fabric.
