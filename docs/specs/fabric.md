# Fabric

Fabric is the daemonless coordination layer for agents on one user's hosts:
durable mail, shared tasks, an activity log, advisory work claims, fenced
landing, bounded task graphs, peer hosts over SSH and the stdio MCP server. It
grants no authority to install, land or run a live provider.

The [cross-cutting decisions](README.md#cross-cutting-decisions) govern shared
contracts and win over this document. ADRs
[0020](../adr/0020-retire-the-daemon-fabric.md),
[0022](../adr/0022-thin-fabric-mcp-execution-facade.md) and
[0026](../adr/0026-federate-fabric-across-own-hosts-over-ssh.md) record the
decisions behind it. Operator setup is in the
[peer host runbook](../runbooks/fabric-hosts.md) and the
[install runbook](../runbooks/install.md#fabric-mcp-registration).

## Purpose and ownership

Three crates sit behind the single `provenant` executable. `fabric-store` owns
the SQLite transactions, `fabric-runtime` owns identity, coordination, peers,
graphs and Git transport, and `fabric-mcp` owns the stdio MCP server. The CLI
and MCP call the same typed service. There is no async runtime, daemon or
listener.

| Caller | Need |
|---|---|
| Agents through MCP | Address an inbox, delegate, wait briefly, read bounded results, resume and cancel precisely |
| User through the CLI | Identify the project, inspect runs, send messages, manage peers and landing |
| Orchestration skills | Correlated requests and replies, owned worktrees, actual route receipts and restartable work |
| Another host | Explicit placement, host-owned status and cancel, committed code transport, bounded output |
| Installer | Register the same installed binary with each harness without disturbing other client configuration |

## Boundaries

Fabric owns messages, tasks, claims, the event stream, landing, graphs, peer
federation and the MCP surface. It delegates the rest.

- **Run** owns the run registry, admission, attempts, status, output, resume,
  cancel and reconcile, and the top-level verbs `dispatch`, `lanes`, `output`,
  `cancel`, `resume` and `reconcile` ([run](run.md)). Fabric keeps IDs and event
  cursors, never a second mutable run status. Run never depends on a Fabric
  crate.
- **Routing, quota and providers** resolve routes and admission. Fabric forwards
  the typed result and never invents a route receipt. A receipt is copied from
  run exactly as `adapter/model@effort`.
- **Supervisor and confinement** own process custody, the writer lock, worktrees
  and safe Git ([supervisor](supervisor.md)). A Fabric work claim is advisory
  and never authority to write a worktree.
- **Config and install** own `config.toml` edits and harness registration.
  `fabric-mcp` owns the registration arguments:
  `provenant fabric mcp --client <label>`.
- **CLI** owns the command envelope, exit codes and exact flags ([cli](cli.md)).

Libraries do not import `cli`. `fabric-runtime` does not depend on `routing`,
`quota`, `telemetry` or `providers` directly; run supplies their aggregate
dispatch and capability APIs. Help and root resolution open no database and
start no thread. Crate layering follows [rust-workspace](rust-workspace.md).

## Contract

### Identity, state and configuration

The state root is host-global and defaults to `~/.local/share/provenant/v1`.
`--state-dir ABS` overrides `PROVENANT_STATE_DIR`, which overrides the default.
No old root is read, and a root nested inside an old one is refused. Fabric
owns `fabric/` (`fabric.sqlite3`, `fetch/`, `locks/`). Run owns top-level
`runs/<RunId>/`.

A host-local `ProjectId` derives from the `HostId` and the canonical Git common
directory, so linked worktrees share it. A non-Git directory uses its canonical
path and stays local. Runtime derives a separate, domain-separated cross-host
`ProjectKey` from the home-relative repository path and verifies it at each
peer. Outside the home directory, remote placement is refused. Moving a
repository creates a new project unless config rebinds it. No scan or URL
heuristic joins projects.

An agent's inbox address is `AgentId` at a host. The label defaults to the
client label, and `PROVENANT_AGENT_LABEL` separates concurrent inboxes. A shared
label shares one read cursor and never competes for claims. Labels do not prove
a model family. Message, task, run and operation IDs are opaque 128-bit hex
values.

Each lane's provider environment and generated MCP entry pin
`PROVENANT_AGENT_LABEL` to `lane-<AttemptId>`, giving it a separate actor and
inbox from the chair. The MCP edge captures the host-set lane environment at
startup. A positive `PROVENANT_LANE_DEPTH` or a present
`PROVENANT_RUN_STATE_DIR` refuses
coordination mutations with `lane_coordination_refused`: mail send and
mark-read, task changes, event notes, claims, landing leases, graph mutations
and submission with an explicit host. The CLI coordination edge also refuses
these mutations, landing push or reconcile, `dispatch --host` and
`workflow session handoff` under that environment. Peer-target run cancel and
resume are also refused. `fabric peer` and `fabric peer-agent` refuse to start
there at all, because a peer frame names its own agent. Reads and child-store
run submit, cancel and output remain available within the existing child
policy. Local resume is refused with `continuation_unsupported` because the
host never launches a child continuation; the CLI returns `UNSUPPORTED` for
every in-lane resume, peer targets included. Graph status
reads an atomic checkpoint without creating graph directories or acquiring the
writer lock.

Lane event reads do not refresh execution events into the shared store and
report incomplete host coverage. Host refresh never reads child stores, so a
lane's own children's events are visible only through `fabric_run` status and
wait.

This guards the tool surface. Lanes still have direct write access to
`fabric.sqlite3` and its companions because opening the current store for a
read requires a writable WAL connection and migration checks.

Settings live under `domains.fabric` in `config.toml` (`agent_label`,
`client_label`, `ssh_executable`, `home_hosts`, `peers`, `limits`); the
bundled defaults are `crates/fabric-runtime/assets/fabric.toml`. There is no
per-knob environment input. Product environment inputs are
`PROVENANT_STATE_DIR`, `PROVENANT_AGENT_LABEL` and the host-set lane inputs
described above.

Limits are hard ceilings that config may lower and never raise. The default
and the ceiling are the same value:

| Limit | Value |
|---|---|
| Message body | 64 KiB |
| Input frame | 1 MiB |
| JSON nesting | 64 |
| Page | 100 rows |
| Output per read | 500 lines or 64 KiB |
| Result envelope | 1 MiB |
| Lease | 1 to 3,600 s |
| Peer call | 30 s |
| Git bundle | 64 MiB |
| Graph timeout | 1 s to 14,400 s |

Every wait takes an explicit budget: up to 14,400 s on the CLI and up to 55 s
over MCP. A met wait exits 0 whatever its targets' outcome; budget expiry never
cancels the underlying run.

### Store

One host-local SQLite database holds all coordination state: bundled rusqlite,
WAL, `foreign_keys=ON`, `synchronous=FULL`, a 250 ms busy timeout per attempt
inside the request deadline, a private 0700 root and 0600 database files, and
numbered embedded migrations (a future `user_version` is refused before any
mutation). Symlinked state components are refused. The database and any `-wal`,
`-shm` or `-journal` companions must be private regular files with one link,
owned by the effective UID; a zero-link observation is re-read, never accepted.
A connection belongs to one worker.

SQLite can create a rollback journal while `PRAGMA journal_mode=WAL` changes
the database header, before Fabric runs migrations. Migrations and transaction
rollback then use WAL. Fabric has no read-only open: even reads open a writable
connection and check migrations. Like run's writable store, Fabric validates
journal metadata and allows a safe journal for recovery. Run's disk-report-only
refusal of non-empty journals does not apply here.

| Table | Meaning |
|---|---|
| `projects`, `agents` | Canonical project key and home host; agent presence as observation, never liveness |
| `messages`, `deliveries`, `mail_cursors` | Immutable messages; one delivery per recipient with a project-local sequence; monotonic per-recipient read cursor |
| `tasks` | Objective, state (open, active, done, cancelled), owner, generation, token, expiry |
| `claims`, `claim_paths` | Fenced work claims and landing leases; canonical path scopes whose prefix overlap is checked in an immediate transaction (`/a` does not overlap `/ab`) |
| `operations` | `(project, id)`, bounded frozen request, digest, phase (prepared, sent, committed, unknown), result |
| `events`, `run_cursors` | Append-only stream with a deduplication key; per-host run-event cursor |

Rules that hold across the tables:

- A mutation and its event commit in one transaction; a failed event write
  rolls the mutation back. No transaction stays open while waiting, spawning,
  probing Git or calling SSH.
- Generation counters survive release and expiry. Claims use the home host's
  clock only, and waits use monotonic deadlines.
- Retryable mutations carry an operation ID. The same ID with a different
  digest is a conflict. A lost reply is answered from the stored receipt and
  never repeats the mutation.
- A busy database is retried with backoff until the request deadline, then
  returns `busy`.
- `mail read --after seq` does not mutate. `mark-read seq` advances only the
  caller's cursor, monotonically and no further than its highest delivered
  sequence. There is no claim or acknowledgement token.
- Task `finish` needs the active fence; changing owner needs release or expiry.
- Run events are pulled lazily into the home store on `events read` and
  `lanes --wait`, deduplicated by `(host, run-event-id)`, with the cursor
  advanced in the same transaction. An unreachable peer yields `Unknown` with
  its host identity and leaves its cursor unchanged. Partial results never
  claim complete coverage.

### CLI and MCP surfaces

[cli](cli.md) freezes exact flags and the JSON envelope. Fabric defines no
other envelope.

| `provenant fabric ...` | Operations |
|---|---|
| `identity` | Project, worktree, host and inbox address |
| `mail send\|read\|mark-read` | Explicit recipients, `--after seq`, per-recipient cursor |
| `task create\|list\|claim\|update\|finish\|release` | Objective and lifecycle; update cannot bypass ownership |
| `events read\|note` | Cursor, limit, optional wait, append-only notes |
| `claim acquire\|renew\|verify\|release\|list` | Advisory issue and path ownership |
| `landing acquire\|renew\|verify\|release\|wait\|push\|reconcile` | Branch, expected SHA and fence; push is an explicit authorised action |
| `graph run\|status\|cancel\|resume` | Bounded task graphs |
| `mcp [--client NAME]` | The stdio server; clients are `claude`, `codex`, `cursor`, `agy`, `opencode` |

Two entry points are hidden from `provenant help`: `fabric peer` (the fixed
peer ingress) and `fabric peer-agent` (the macOS LaunchAgent relay). `--json`
is refused by `fabric mcp` before it reads stdin, and a bare `provenant` prints
usage and exits 2, so a registration must name `fabric mcp`.

The MCP server exposes 11 tools. Each takes a tagged `op` enum that rejects
unknown fields, with no numeric-string coercion or synonym repair. The schema
catalogue has two cached forms: strict schemas for serde acceptance probes and
error paths, and compact schemas for `tools/list`. Every valid strict-schema
probe must also pass its compact schema.

| Tool | Operations |
|---|---|
| `fabric_identity` | inspect |
| `fabric_mail` | send, read, mark_read |
| `fabric_task` | create, list, claim, update, finish, release |
| `fabric_events` | read, note |
| `fabric_claim` | acquire, renew, verify, release, list |
| `fabric_landing` | acquire, renew, verify, release, wait |
| `fabric_submit` | submission |
| `fabric_run` | list, status, wait, cancel, resume |
| `fabric_graph` | run, status, cancel, resume |
| `fabric_output` | read, fetch |
| `fabric_hosts` | list |

The compact input schemas keep `maxLength` and identifier patterns for IDs,
operation IDs, Git OIDs, SHA-256 digests and graph IDs. They omit text
`minLength`, UTF-8 byte annotations and the NUL-exclusion pattern. Runtime
validation still rejects empty text, NUL characters and excess UTF-8 bytes
before side effects, with `invalid_input` and the exact field path. Since a
string's code point count cannot exceed its UTF-8 byte count, these schemas
only relax acceptance. Single-use definitions are inlined recursively;
unreachable definitions are removed. Definitions used at least twice stay in
`$defs` at the input-schema root, and an empty `$defs` is omitted.

Only identity and submit publish `outputSchema`, each with `type: "object"`.
Submit's optional `worktree` receipt is an object with `path` and `branch`.
Other tools still send `structuredContent`. `fabric_hosts` returns
`{"hosts":[...]}`, including `{"hosts":[]}` for an empty page. The CLI host
list keeps its JSON `data` array; peer health replies keep the single
`HostHealth` record. The wrapper belongs only to the MCP edge. This also
satisfies the object requirement in MCP 2025-11-25; MCP 2026-07-28 permits any
JSON value.

The identity reply adds `server_version`, the MCP server's package version. A
reinstall does not restart a running server, and the CLI keeps no record of
running MCP servers, so agents compare `server_version` with
`provenant --version` (see the orchestrate identity reference).

The shared submit wire type accepts `new_worktree` for peer ingress. The MCP
submit owner refuses worktree creation, so MCP parsing rejects that field,
including `null`, with `invalid_input` at `arguments.tasks[N].new_worktree`.
It remains absent from the input schema. Peer deserialisation and worktree
creation through CLI dispatch are unchanged.

Both the [2025-11-25 output-schema rules](https://modelcontextprotocol.io/specification/2025-11-25/server/tools#output-schema)
and the [2026-07-28 rules](https://modelcontextprotocol.io/specification/2026-07-28/server/tools#output-schema)
require structured results to conform to a published output schema, without
an explicit exemption for `isError`. Fabric therefore omits
`structuredContent` on identity and submit errors. Their `content` text
contains the stable code and JSON error details, including the field path
for invalid input and `operation_id` for `launch_unknown`. This avoids
validating error details against a success schema or inventing a success
receipt after an uncertain launch. Other tools retain structured error
details; a frame-size fallback whose tool is unknown uses text only.

Legacy catalogues omit all output schemas. Both protocol eras use the same
compact input schemas and error policy; protocol versions and catalogue
cache behaviour are unchanged.

Issue #1571 wave 1 measured the following bytes with
`serde_json::to_vec(tool).len()`, including each complete tool object and
excluding catalogue array framing. The total fell by 75.8%. Tests allow 10%
growth per tool, rounded up, with hard caps of 3,072 bytes per tool and 18 KiB
for the sum.

| Tool | Before (bytes) | After (bytes) |
|---|---:|---:|
| `fabric_identity` | 5,973 | 1,387 |
| `fabric_mail` | 6,586 | 2,441 |
| `fabric_task` | 7,710 | 2,209 |
| `fabric_events` | 6,220 | 781 |
| `fabric_claim` | 6,776 | 2,185 |
| `fabric_landing` | 7,105 | 2,020 |
| `fabric_submit` | 6,780 | 2,338 |
| `fabric_run` | 7,545 | 1,776 |
| `fabric_graph` | 5,780 | 838 |
| `fabric_output` | 7,395 | 1,375 |
| `fabric_hosts` | 5,188 | 336 |
| **Total** | **73,058** | **17,686** |

Mutating operations in the mail, task, events, claim and landing tools accept
an optional `operation_id` so a caller can retry after a lost reply; observation
operations reject it. Responses carry typed `structuredContent` and a short text
digest of IDs, state, the exact route if known and the next cursor. Output is
UTF-8 text when valid and an explicit byte array otherwise, never lossy.
`stdout` and `stderr` are separate streams with no combined ordering. `fetch`
copies only run-exported regular files, refuses symlinks and traversal, enforces
byte and file caps and verifies a manifest digest before atomic publication
under `fetch/<host>/<run>/`. Output tools accept no arbitrary source or
destination path.

Submission tasks carry a prompt and route preference, with optional account,
access, worktree, `deliver` and delivery ID, and a required `timeout_ms` (1000
to 14400000). They accept no artefact inputs. `resume` is separate from
submission and cannot change route, account or authority.

### Bounded graphs and route fallback

A graph document has `schema_version: 1`, `graph_id` and `nodes`. A node has an
`id`, a `prompt`, optional `depends_on`, an `access` of `read_only` or
`worktree_write` (a writer names its registered worktree) and exactly one of
`route` or the ordered `routes`. A route holds an `adapter`, one of `model` or
`alias`, and optional `effort` and `account`. `account` selects a registered
provider account, with the same meaning as `dispatch --account`; each fallback
route can select a different account. Limits are 8 routes per node, 64 nodes,
256 edges, 64 KiB per prompt and 256 KiB of prompt in total. `run` and `resume`
take a project-local document path, and `status` and `cancel` take the graph
`id`. Each graph attempt is bounded by `limits.graph_timeout_ms`.

`run` starts ready nodes and returns. After a run settles, its existing
supervisor advances pending graphs in the same project, after releasing
attempt custody and writer locks. Dependants start without a chair tick or a
separate daemon. The journal retains the document and original working
directory, so advancement does not depend on the submitting process or on
later edits to the document file. These host-only inputs are omitted from
`status` replies. A later settlement also recovers a graph whose advancing
process exited. If no further run settles, `resume` is the manual recovery
path; it validates the supplied document against the checkpoint and records the
directory it ran from.

The supervisor's output is discarded, so a failed advance is written beside
the checkpoint as `graph-<id>.advance-error`. `status` reports it as
`advance_error`. The next successful advance, or a successful `run` or
`resume`, removes it.

Nodes started by settlement record the supervisor's own identity as their
`launch_actor`, not the chair's, so `cancel` from the chair cannot recover
an attempt whose reference was never saved. They reuse the settled attempt's
host launch inputs. Graph writers do not deliver (`deliver` is unset): the
chair lands a writer's worktree.

- The owner writes a starting checkpoint before it calls run, under a stable
  key for graph, node and route index. A pending or unknown observation reuses
  the key, and run's actor-scoped operation replay returns the existing attempt
  instead of creating a second.
- Only a terminal `auth_required`, `usage_limited` or `adapter_unavailable`
  result consumes the next route. Task failure, input requirements and
  uncertain launches never fall back. After a provider launched, only read-only
  nodes may fall back. A failed writer keeps its worktree in the checkpoint for
  the chair, and missing launch evidence counts as launched.
- Dependants start after their prerequisites succeed and block after one fails.
  Every tried route and its outcome stays in the checkpoint. Plan and
  checkpoint digests, a per-project per-graph lock and atomic publication make
  ticks restartable. Cancel persists the admission fence before it asks run to
  cancel accepted attempts, and resume checks the document digest.
- Writer nodes use run's ordinary path: the shared submission path verifies the
  project registration and acquires the writer lock before it compiles
  confinement. A terminal Git sequencer check can turn provider success into a
  failed attempt. Lost custody keeps the reservation for chair recovery
  ([worktrees](../worktrees.md#writer-custody-recovery)).

### Waits

`lanes --wait` uses `WaitCondition { Any, All }`. Any returns on a terminal
target, All when every selected target is terminal, and either returns at once
on `InputRequired`. The target set is frozen at invocation. Long CLI waits loop
bounded reads under one monotonic deadline, refreshing events between reads. A
peer read cut short by the wait's own deadline after a completed poll returns
the last attempts as `timed_out`, not as an unavailable peer. Landing `wait`
observes a supplied generation and returns on release or a holder change. It
never acquires the lease.

### Landing

Landing runs entirely on the home host under a per-ref OS lock. The lease binds
a generation and token to the expected remote OID and the candidate OID. Before
pushing, the operation is marked `pushing`, and expiry cannot admit a successor
while it is unresolved. The push proves the candidate is a fast-forward, then
uses Git's exact `--force-with-lease=<ref>:<expected>` fence. It is never
authority for a non-fast-forward rewrite. Push records bind the resolved origin
URL as well as the ref and OIDs, and URLs that carry passwords or query or
fragment credentials are refused before anything persists. Renewal, release and
push each compare holder, generation and token. No SQLite write lock is held
across Git.

### Peer hosts

Fabric works across the user's own Macs as one system. A chair on one host
dispatches to another and sees every host's lanes in one view. Each host keeps
running its own lanes when the others sleep or go offline. The first
deployment is a laptop and an always-on spare Mac reached over SSH, usually
through Tailscale.

**Scenarios.**

| Scenario | Required behaviour |
|---|---|
| Peer offline | Local work is unchanged. Peer rows are `Unknown` with the host identity, never `dead` or `orphaned`. A default-placed lane that cannot reach its host before the request is sent runs locally and says so. |
| Peer on, chair host off | Peer lanes run to completion and record results. The home chair reads their runs and events when it returns. |
| Different networks or a direct cable | The same commands. The SSH host alias resolves the route. |
| Chair on the always-on host | Configuration is symmetric: that chair sees the laptop whenever it is reachable, and can land work when it is the home host. |

**Model.**

- A host is a named machine in config with a stable host ID and an SSH
  destination. Host IDs and display labels are separate.
- Each host is authoritative for the records it created: its runs, store rows
  and processes. No host writes another host's store except through that host's
  peer ingress. Only the owning host reports process identity or liveness, so
  an unreachable peer is `Unknown`. Peer snapshots are not cached.
- Each project names one home host, default local. Messages, tasks, events,
  claims and landing share that authority, and a non-home host makes no local
  mirror writes. Send, read and mark-read all target the home, including for a
  recipient on another host. An unreachable home returns `HomeUnavailable`
  while local run execution stays available. There is no outbox, merged clock
  or home election. Remote workers return results only through runs, and the
  home chair pulls their events. Reassigning the home refuses active ownership.
- A federated read queries every reachable host and merges rows with a `host`
  field. A write to a record on another host runs on its owner. Qualified IDs
  carry the owning host, and a selector that matches several hosts is refused
  as ambiguous rather than resolved to the first match.

**Transport.** Peers are the same user's trusted hosts reached by system SSH,
with no network listener and no credential of Provenant's own. The remote
command is the fixed `provenant fabric peer`, which accepts one versioned,
framed request and never a caller-built command string. SSH destination and
executable overrides come only from validated config (an absolute SSH
executable, and per peer an absolute remote executable and a connect timeout of
1 to 30 s). Supervisor captures bounded stdin, stdout and stderr under a fixed
argv and deadline. The ingress also carries Git object transfer, so one SSH key
restricted with `command=` supports every peer operation. The handshake checks
the protocol major, host ID and `ProjectKey`; a version mismatch refuses writes
and flags reads. Fan-out uses at most four transport threads, holds no database
transaction and never recurses from a peer request. Cancelling a peer call stops
the SSH child, not the remote run.

Requests carry the version, target and source hosts, the agent label and a
home-relative locator bound to the `ProjectKey`. Ingress checks the source
against its configured hosts, refuses its own host ID and resolves its own
`ProjectId`. Only local run events are ingested and other hosts are reported as
unknown. The reply uses `unknown_hosts` for coverage gaps and `run_events_pending`
when another page is available.

**Remote mutations.** A mutation is prepared durably, marked sent before the
transport and recorded committed or unknown afterwards. A repeated uncertain
operation asks only for the home's atomic receipt. It never repeats the
mutation and never falls back locally. A launch whose request was sent but not
answered is `launch_unknown`; the caller reconciles by operation ID on the next
contact. An explicitly selected host never falls back. A lane never moves hosts
after it starts. Remote dispatch accepts the same request fields as a local one.
Prompts travel in the request body, absolute paths under the user's home are
rewritten home-relative, and any other absolute path is refused.

**macOS peer agent.** A process started from SSH cannot read the login
keychain, so Claude reports no login there. The installer starts a per-user
LaunchAgent, `dev.provenant.peer`, in the GUI domain (`fabric peer-agent`). It
listens on a Unix socket in a 0700 `peer-agent/` directory under the default
state root and relays each request to a fresh `fabric peer --in-process` child,
so the request, its credential reads and its lanes run in the login session.
The SSH entrypoint forwards its frame when the agent accepts and handles it in
process otherwise. Only a refused or missing socket falls back. A request that
was sent is never repeated, and a lost reply fails the call so a mutation
reconciles as `launch_unknown`.

**Code transport.** A peer writer takes its base from the shared remote, not
from the caller. The submission names a new branch and a base, and peer ingress
refuses a caller-supplied path. The peer fetches every remote with its own Git
authority and hooks disabled, refuses a base no remote-tracking ref contains
(`base_unreachable`), pins it to a commit and creates the branch's registered
worktree through its worktree owner. A retry adopts a worktree that is clean at
the pinned commit with no live writer, and otherwise an existing branch is
`branch_exists`. The writer then takes the ordinary local writer path on the
peer, with the peer's credentials. The receipt names the peer worktree, where
undelivered commits stay. Uncommitted changes are never transported. Committed
bundles and digest-checked chunk fetches honour `limits.bundle_bytes`.

**Credentials.** Provenant never copies, forwards or reads provider
credentials, SSH keys or keychains across hosts. Sandbox and capability policy
apply on the executing host exactly as for a local lane. A host with only some
adapters signed in is a complete peer, and no operation depends on a provider's
own remote feature.

**Handoff.** `workflow session handoff` starts a fresh session on a peer from a
generated prompt through one `session_handoff` peer action
([workflow](workflow.md#session-handoff)).

## External contracts

Fabric does not own these wire forms.

- **MCP.** The server is an in-house, bounded, tools-only stdio JSON-RPC
  transport with UTF-8 newline framing, protocol-only stdout and diagnostics on
  stderr. It serves two protocol eras and refuses any switch within a
  connection.

  | Version | Behaviour |
  |---|---|
  | [2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle) | `initialize`, `notifications/initialized`, version and capability negotiation, `ping`, `tools/list`, `tools/call`, `notifications/cancelled` |
  | [2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning) | No `initialize` or `ping`; per-request `_meta` keys `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities`; `server/discover`; results carry `resultType` (`complete`), and `tools/list` carries `ttlMs` and `cacheScope` |

  An unsupported version returns `UnsupportedProtocolVersionError` with the
  supported list. No resources, prompts, sampling, subscriptions or tasks are
  advertised. JSON-RPC errors stay distinct from tool failures (`isError`), and
  string and integer IDs are preserved. Cancellation suppresses the late reply
  and never cancels a launched run. Recorded first-request fixtures for Claude
  Code and Codex under `crates/fabric-mcp/tests/fixtures/` pin each era.
- **Hooks.** Hook stdin and stdout follow each harness's native protocol and are
  registered through cli's `HookSpec` table ([cli](cli.md)).
- **Git and SSH.** System Git and OpenSSH, with the environment sanitised as
  [supervisor](supervisor.md) describes.

## Failure and recovery semantics

- **Concurrency.** One MCP process serves one client: a bounded stdin reader,
  one stdout writer and four request workers on a queue of 32. Cancellation and
  protocol control use a separate bounded path, and saturation returns a typed
  busy error. Frames are read up to the cap plus one byte before refusal, and
  duplicate live request IDs are refused. The writer has a 30 s deadline. On
  EOF or a read error, admission stops, accepted work drains for at most 55 s
  and cooperative observations are cancelled and joined within a further 1 s.
  Detached run owners survive all of this.
- **Run custody.** Before it acknowledges a submission, run stores the frozen
  request, resolved route and access, operation digest and attempt IDs. An
  accepted receipt means durable admission, not that a provider started.
  `run::recover` replays an accepted operation only when no provider-launch
  intent was recorded. After that boundary, recovery observes or reports
  unknown and never replays. A held lock, live provider or uncertain identity
  means starting or unknown, never a second launch. An uncertain attempt stays
  unknown. Any retry is a new authorised operation.
- **No kill on silence.** Silence, an MCP disconnect, a bookkeeping failure or
  ENOSPC never stops a healthy provider. Only explicit cancellation, an
  authorised timeout or the run owner's custody rules can. Missing or malformed
  evidence is unknown, never success, and a bare PID is never a cancellation
  capability.
- **Landing.** On a lost response or owner death, the lease stays blocked and
  the remote OID is reconciled. The candidate means committed, the expected
  value means a retry may be requested, and anything else is a conflict. Absent
  proof stays unknown and cannot release the fence.
- **Errors.** Libraries return typed `thiserror` enums without raw secrets.
  Expected contention, stale fences and uncertainty are data. The stable service
  codes are `invalid_input`, `not_found`, `conflict`, `busy`, `stale_fence`,
  `home_unavailable`, `peer_version`, `launch_unknown`,
  `continuation_unsupported`, `confinement_refused`, `lane_depth`,
  `lane_coordination_refused`,
  `worktree_missing`, `base_unreachable`, `branch_exists`, `output_too_large`,
  `state_corrupt`, `upgrade_refused`, `publication_failed`, `cancelled`,
  `timeout` and `io`.
  Uncertain mutations include their `operation_id`. Oversized read responses
  return `result_too_large`, while committed mutation receipts keep the
  transport ceiling so a lowered read limit cannot hide them. Health checks do
  not repair, prune or acquire claims.

Peer errors use the locally frozen request's operation ID. A supplied peer ID
must match it; generic refusals may omit the ID. A mismatch is a protocol error
(`state_corrupt`). A remote status snapshot must name the requested attempt,
as must each record returned to a wait or cancel. A resume receipt must name
the requested continuation operation. Submission receipts must name the
requested operation and placement; a peer writer must return the requested
branch, and a read-only submission cannot return a worktree. Landing push and
reconcile receipts must name the requested operation too.

Human lane listings remove control, escape and formatting characters from peer
text through the shared plain-text sanitiser; structured records preserve the
received values.

## Follow-up: lane store isolation

`lane-store-isolation` will add a read-only coordination store path and move
remaining shared-store writes to the host. It must preserve
local and federated reads and child-store submission without granting lanes
write access to shared SQLite or `fabric/locks`, then remove those grants.

## Deliberate cuts

| Cut | Reason |
|---|---|
| Named sessions and ad-hoc handoff prompt assembly | Two sessions in all recorded history; run `resume` covers continuation, and the chair submits an explicit prompt |
| Teams and membership tables | Explicit recipient lists replace a second address registry |
| Mail claim, expiry and acknowledge | Per-recipient read cursors replace them, and distinct labels separate consumers |
| Task dependency API | Four task states and token-fenced ownership; the chair submits dependent runs explicitly |
| Separate notes, activity, events and watch views | One append-only event stream with bounded cursor reads |
| Terminal-run mail notices by default | Run completion emits an idempotent event; mail only when requested |
| Fabric-owned routing pools, councils, rotation, provider catalogue and cooldown reset | `routing` resolves once and Fabric forwards; `route list` shows the effective catalogue and pools after config overrides |
| Native-seat provider bans | No family is inferred from a seat; shared run policy governs dispatch |
| Graph shell gates, gate workers and automatic task mirroring | Verification is an ordinary authorised task or chair action |
| Host auto-placement and local fallback after an explicit choice | Placement is local by default and remote only when explicit |
| Remote PID probing | Only the owning host can report process identity |
| Offline outbound mailbox, inbox pull and merged host cursors | One authoritative sequence at the home; no replicator or clock-skew merge |
| Copying whole run directories | An explicit verified fetch of an allowlisted artefact set replaces it |
| Registration doctor and install smoke | `check` subjects and `hosts` readiness cover them |
| Startup retention sweeps and DB pruning | Explicit maintenance of new state only |
| Numeric coercion, synonym repair and fuzzy command hints | Invalid fields are refused with a stable code and field path |
| Python and Node discovery, subprocess RPC and package provisioning | Everything runs in process in one executable |
| Importing legacy state at runtime | The old store is left untouched and no command reads it |
| The `rmcp` SDK | It would need `tokio`, `tokio-util`, `futures` and `tracing` and reverse the no-async rule; a single tools-only stdio server does not justify that. Revisit if HTTP, elicitation or subscriptions become real requirements |
| Automatic load-based placement, moving a running lane, shared or replicated SQLite, any daemon or network listener, non-macOS hosts, other people's hosts, mapping different checkout paths | Outside the supported peer model; identical home-relative checkout paths are required |

## Verification

Tests prove behaviour through the built binary and the crates, with isolated
state roots and fake providers and SSH. Nothing needs a second language runtime.

| Area | Where | What it proves |
|---|---|---|
| Store | `crates/fabric-store/src/tests*`, `tests/fixtures` | Cursor isolation, task fencing, path-overlap boundaries, generation monotonicity, event rollback, future-schema refusal, concurrent cold opens and claims with one winner, forced `SQLITE_FULL` and `BUSY` |
| Runtime | `crates/fabric-runtime/src` (including `landing_tests.rs`) | Identity, configuration bounds, graph checkpoints and fallback, peer ingress refusal, replay-safe run-event ingestion, Git transport and fenced landing against real temporary repositories, including a lost push reply |
| MCP | `crates/fabric-mcp/src/*_tests.rs`, `tests/fixtures` | Both protocol eras and first-message selection, `tools/list` schemas, strict calls, size and depth errors, cancellation, a full queue, EOF and a broken stdout, using the recorded client fixtures |
| Binary | `tools/front-door/tests/fabric.rs`, `fabric_transport.rs` | Fabric subtree and top-level run verbs, the envelope, waits, interpreter-free MCP negotiation and mail, and forced peer commands with a fake SSH executable |

The tests do not establish two-host acceptance. Exact-head CI and a chair-run
smoke between two real hosts remain required for peer changes.
