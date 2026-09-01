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
fabric inbox                         # claim available deliveries (default: 20)
fabric inbox --limit 5               # claim at most five deliveries
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
fabric_batch
```

`fabric_dispatch` accepts one inline prompt or prompt file. `fabric_batch`
accepts 1–64 fixed tasks with concurrency capped at eight. Both default to the
current provider seat, the `workhorse` route and the `worker` role. They create
the run directory automatically, delegate to `dispatch_run.py` or
`batch_run.py`, and return compact status, route and absolute artifact paths;
full prompts, results and diagnostics remain file-backed. `wait_seconds: 0`
returns immediately, while values through 55 wait within one MCP call.

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
