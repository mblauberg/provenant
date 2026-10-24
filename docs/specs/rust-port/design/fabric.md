# Fabric Rust design

Status: proposed, conformed 24 October 2026. Audience: the two Rust chairs and Fabric
implementers. This document defines the replacement, its dependency requests and
its deletions. It approves no install, import, live-provider run or landing.

## Direction and boundary

Build Fabric as three libraries behind the single `provenant` executable:
`fabric-store` owns transactions, `fabric-runtime` owns coordination and the
execution facade, and `fabric-mcp` owns stdio MCP. Both CLI and MCP call the same
typed service. Keep durable messages, tasks, fenced ownership, detached runs
and explicit SSH peers. Keep bounded graph execution with ordered route fallback.
Remove named sessions, interpreter bridges,
parallel status engines, permissive input repair and duplicated routing policy.

The [binding cross-area decisions](https://github.com/mblauberg/provenant/blob/docs/rust-port-slices/docs/specs/rust-port/design/README.md#cross-area-decisions)
govern shared contracts and win over conflicting area prose. [PORTING v7](https://github.com/mblauberg/provenant/blob/docs/rust-port-slices/docs/specs/rust-port/PORTING.md)
supersedes transition/parity rules; the dependency allowlist, crate layering,
typed errors, serde records, bounded inputs and `supervisor::sys` unsafe boundary
still apply. There is no async runtime. Only provider wire forms, harness hook
protocols, Agent Skills format, MCP and Git survive as external contracts.
Our verbs, tools, records and layouts change together. The installed product
root stays frozen until cut-over under F0. Delete each old owner and its tests
when its replacement and retained callers land; install once after integration.
Do not build selectors, adapters, recorded old-runtime judges or differential
oracles. Native verification below replaces old parity gates. Leave old
state stores and transcripts untouched; cut-over only reads the import source.
`telemetry stats` reduces run terminal records with the price table, without a
ledger. The old ledger is neither imported nor deleted; the OTel collector retires.

Source baseline: `3eb3a0b5`. The fetched `docs/rust-port-slices` SLICES cards
31–38 are an inventory, not a delivery plan. Counts below are physical lines at
that baseline. The unmerged diffs of [#1332](https://github.com/mblauberg/provenant/pull/1332),
[#1334](https://github.com/mblauberg/provenant/pull/1334) and
[#1344](https://github.com/mblauberg/provenant/pull/1344) are reuse candidates,
not prerequisites to merge wholesale.

## Users and evidence

| Caller | Need | Current evidence |
|---|---|---|
| Agents through MCP | Address their inbox, delegate, wait briefly, retrieve bounded results, resume and cancel precisely | `runtime/fabric/src/server.ts:59` registers the server; `src/tool-schemas.ts:51` starts the 20-tool catalogue |
| User through CLI | Identify the project, inspect runs and failures, send messages, manage peers and landing | `README.md:92,148`; `tools/front-door/src/main.rs` routes Fabric, lease, hosts, lanes, events and graph |
| Orchestration skills | Correlated requests and replies, owned worktrees, actual route receipts and restartable work | `skills/orchestrate/SKILL.md:11–23` names dispatch, status, continuation and output; dispatch/batch execution stays with `run` |
| Hooks and workflow helpers | Stable hook stdin/stdout, bounded context and notifications | The CLI-owned `HookSpec` registry defines recorded hook protocols; legacy Python hook configuration retires at state cut-over. `scripts/routing_context_hook.py` is legacy survey evidence, not an installer API. |
| Another host | Explicit placement, host-owned status/cancel, committed code transport and bounded output | `scripts/fabric_hosts.py:250,700`; `scripts/fabric_lanes.py:18`; `scripts/fabric_git.py:24` |
| Installer and six MCP clients | Register the same installed binary while preserving unrelated client configuration | `scripts/configure-fabric-mcp.py:168`; [registration runbook](../../../runbooks/fabric-mcp-registration.md) |
| Other library owners | One run/status API and one policy loader | `fabric_policy.py` is also imported by lint, secret scan and provider execution; its filename does not make those policies Fabric-owned |

The source paths abbreviated as `src/` in this table are below `runtime/fabric/`.
The static call-site scan covers skills, hooks, scripts and docs. Observed usage
and the resulting retention decisions are recorded below; static references do
not establish that a feature is used. Local inspection reads metadata only,
never credentials, prompt bodies or transcript content.

Rechecked on 24 October 2026 with one read-only SQLite snapshot of
`~/.local/state/agent-harness/fabric/fabric.sqlite3`. Counts below exclude
projects matching `project LIKE '%/T/%' OR project LIKE '%/tmp/%' OR
project='/private/tmp'`, unless labelled raw. These temporary projects include
lifecycle tests that wrote to the live root; they are not product adoption.

| Observation | Count | Design implication |
|---|---:|---|
| Messages / delivery rows / delivery claims | 10,955 / 16,680 / 541 | Keep durable mail, simplify its read state |
| Message kinds | 6,535 terminal, 2,777 response, 431 request | Terminal notices become events; retain correlated mail |
| Agents | 1,099 | Labels identify inboxes, not active providers |
| Temporary-project pollution (raw subset) | 1,508 of 12,463 messages; 1,509 of 2,608 agents | Every test must use an isolated state root |
| Mail processing (raw) | Response deliveries 5,336, unread 5,150; request 1,157, unread 1,084; 136 acks | Replace claim/ack with per-recipient read cursors |
| Message metadata (raw) | All 4,422 non-terminal messages have kind; 1,176 have output_path | Preserve kind and typed artefact references |
| Tasks | 475, including 25 open | Keep tasks; import open and blocked work at cut-over |
| Activity / work claims | 16,484 / 454 | Keep one journal and advisory ownership |
| Landing activity | 1,386 acquires/releases; 1,355 acquires in sealle-app | Keep bounded landing wait |
| Run observations / qualified remote observations | 8,252 / 3,338; 2,011 distinct remote IDs, including 786 workshop IDs | Keep federation and owning-host status |
| Sessions / session events | 2 / 13 | Cut named sessions; `resume` covers continuation |
| Graphs / attempts / reservations | 3 / 11 / 0; two stopped, all created 22–23 October while building the runner | Retain a bounded DAG owner after the 24 October cut-over trial required ordered route fallback |

Counts establish recorded activity, not current liveness. Reproduce with
`mode=ro`, a read transaction, the predicate above and `count(*)` over the named
tables; join deliveries to messages for kinds and read state. Group activity by
kind, and count qualified IDs in run_observations. No bodies, prompts,
transcripts or credentials were inspected. Telemetry history remains with the old install.

Additional closure evidence: `writer_lease.py:201–239` reads graph reservations
directly; remove that DB coupling and rely on run's real writer lock.
`dispatch_preflight.py:517–580`, `provider_confinement.py:197–201` (both under
`skills/orchestrate/scripts/`) and `scripts/telemetry_hosts.py:21–23` use host
helpers. Their Rust owners must consume typed host/temporary-path APIs before
those Python owners disappear; do not leave a cross-language shim.

## Feature decisions

`Surface` marks an intentional change visible to a caller. Every such change
requires caller edits in the same replacement lane, not an alias layer.

| Existing feature | Verdict | Replacement and reason | Surface |
|---|---|---|---|
| cwd identity, common Git project across worktrees | Keep | Host-local ProjectId from canonical Git common directory; runtime owns a separate cross-host project key | New IDs |
| Seats, provider labels and agent inbox labels | Simplify | Explicit client label plus optional agent label; neither proves model family | Environment changes |
| Mail, reply correlation, task links | Keep | Immutable message plus per-recipient delivery; bounded body and explicit references | Typed envelope |
| Peek, claim, expiry, acknowledge | Simplify | Per-recipient read cursor; distinct labels separate consumers | `read --after seq`, `mark-read seq`; no claim/ack |
| Terminal-run mail notices | Merge | Run completion emits an idempotent activity event; mail only when requested | Fewer unsolicited messages |
| Teams and membership tables | Cut | Explicit recipient lists replace a second address registry | Team addressing removed |
| Tasks, ownership, done and dependencies | Simplify | Four states and token-fenced ownership; chair submits dependent runs explicitly | Task dependency API removed |
| Notes, activity, events and watch | Merge | One append-only project event stream with bounded cursor reads | Four views become one |
| Advisory issue/path work claims | Keep | Generation-fenced claims in the project home store; canonical path scopes | New claim operations |
| Landing lease, generation and expected SHA | Keep | Store fencing plus a home-host push operation and Git lease | New landing subtree |
| Run registry, seat index, recursive lane scans | Confirmed | `run` owns registry, admission, attempts and status; runtime owns identity and federated query routing | [#1356](https://github.com/mblauberg/provenant/pull/1356), PORTING R2 |
| Status, lanes, runs, dispatch list and wait | Merge | One run query plus bounded wait; missing proof stays unknown | Aliases removed |
| Output tails, events and receipts | Simplify | Named artefact enum, byte cursor and cap; no arbitrary path read | New output shape |
| Local/remote fetch | Keep | Explicit verified copy of an allowlisted run artefact set | No whole-directory copy |
| Single dispatch and batch | Merge | One submission containing a task list; run owns submission and admission limits | Scalar dispatch form removed |
| Routing pools, councils, rotation and fallbacks in TS | Merge | `routing` resolves typed selections once; Fabric forwards the result | No Fabric-specific precedence |
| Provider catalogue and cooldown reset | Cut from Fabric | `provenant route list --adapter <id>` replaces live-ID discovery; routing/quota own administration | `provenant route list` removed |
| Dispatch route and authority validation | Merge | `run`, `routing` and `confinement` own it; Fabric validates envelope and project only | Strict typed inputs |
| Native-seat provider bans | Cut | No family inferred from a seat; shared run policy controls permitted dispatch | Same-provider dispatch possible |
| Named sessions | Cut | Only 2 sessions / 13 events ever recorded; native continuation stays in `resume` | Remove session table/tool/flags and F4 |
| Ad-hoc handoff prompt assembly | Cut | Chair submits an explicit new task with artefact inputs | `handoff` removed |
| Bounded graph execution | Keep | One bounded DAG owner delegates attempts to run and checkpoints every attempt | Single `route` or ordered `routes`; terminal availability failures try the next route |
| Graph shell gates, gate workers, auto task mirroring | Cut | Verification is an ordinary authorised task or chair action | Gate document fields removed |
| SSH hosts and diagnostics | Keep | Explicit trusted host list and one fixed peer command | Versioned new peer envelope |
| Host auto-placement and local fallback | Cut | Default is local; remote placement always explicit | Placement defaults removed |
| Remote launch/cancel/resume idempotency | Keep | Durable operation ID and request digest, unknown launch never retries locally | New operation receipt |
| Remote PID probing | Cut | Only the owning host can report process identity/liveness | Unreachable peer is `Unknown` |
| Messages/tasks/activity federation | Simplify | Route coordination to one project home, never merge replicated stores | Offline remote coordination refused |
| Offline outbound mailbox, inbox pull and merged host cursors | Cut | No outbox replicator or clock-skew merge; one authoritative sequence | Offline delivery removed |
| Git bundles and bound writer results | Keep | Committed base, isolated refs, verified head and worktree ownership | Explicit transport request |
| Registration, doctor and install smoke | Merge | `fabric-mcp` owns registration/descriptor; config owns edits; CLI verification uses existing `check` subjects, readiness is list data | Explicit `fabric mcp` args; doctor removed |
| DB diagnostics and pruning | Simplify | Typed store faults; explicit maintenance of new state only | No startup retention sweep |
| Numeric coercion, synonym repair, fuzzy command hints | Cut | Refuse invalid fields with a stable error code and field path | Inputs become strict |
| Python/Node discovery, subprocess RPC and package provisioning | Cut | In-process libraries and one executable | No runtime interpreters |
| Old MCP judges and Rust differential tests | Cut | Rust-native contract fixtures test the new design | Old transcripts retired |

## Cut-over decisions, 24 October 2026

Lane C is attributed to `codex/gpt-6.1-sol@high`. The user authorised
cross-crate API changes needed by the retained Fabric surfaces. CLI subtree
bindings are included in that scope; the binary and CLI use the same runtime
home router. Python `fabric_*` deletions belong to the separate python-cut lane.

Peer ingress is the fixed `provenant fabric peer` command over system SSH.
Requests carry version, target/source host, agent label and a home-relative
canonical common-directory locator bound to the cross-host key. Ingress checks
configured source hosts and resolves its own local project ID. It never forwards
another peer request. Outbound mutation bytes and digest are durably prepared,
marked sent before transport and recorded committed or unknown afterwards.
Repeating an uncertain operation asks only for the home's atomic receipt; it
never repeats the mutation or falls back locally. Coordination receipts retain
the original actor. Run-event ingestion commits host-qualified deduplication and
cursor advancement together. Ingress only ingests its local run events and
reports other configured hosts as unknown; it never recursively fans out. Peer
failures remain explicit observations.

Landing push records bind the resolved origin URL as well as the ref and OIDs.
URLs containing passwords or query/fragment credentials are refused before
persistence; validated SSH account names remain supported. The lock directory is private and lock files persist: supervisor
verifies inode identity on acquisition and the store verifies the directory
binding. This avoids adding an alternate lock implementation in Fabric.

The MCP binary uses the existing four-worker transport. Its OS stdin reader
owns a one-slot bounded queue; the transport-facing reader observes deadlines.
Stdout uses supervisor's nonblocking deadline writer. A blocked OS stdin reader
ends with the MCP process and has no provider custody. Disconnect and observation
cancellation never become run cancellation.

### Bounded graphs and route fallback

A graph document has `schema_version: 1`, `graph_id` and `nodes`. Each node has
`id`, `prompt`, optional `depends_on` and exactly one of `route` or `routes`.
The single-route form stays valid. A route retains `adapter`, exactly one of
`model` or `alias`, and optional `effort`. `routes` is ordered, with at most eight
entries; the owner never invents a model or route receipt. Graphs have at most
64 nodes and 256 edges, with 64 KiB per prompt and 256 KiB total prompt bytes.

The graph owner writes a starting checkpoint before invoking run with a stable
key for graph, node and route index. Pending and unknown observations reuse that
key. Only a terminal `auth_required`, `usage_limited` or `adapter_unavailable`
result consumes the next route. Task failure, input requirements and uncertain
launches do not trigger fallback. Every tried route and its terminal outcome
remains in the checkpoint. Dependants start after successful prerequisites and
become blocked after a failed prerequisite. Canonical plan/checkpoint digests,
a per-project, per-graph lock and atomic publication under new state make ticks restartable.
No shell gates or named sessions return.

The run API remains the owner of launch, custody, status, output and controls.
If its dispatch/control service has not landed when F3 binds, the binary uses the
minimal typed Fabric trait and reports `owner_unavailable` for unsupported owner
operations. That binding is an explicit integration TODO, never a successful
launch receipt. Existing run records remain authoritative for supported reads.
The binary binds status, frozen-target waits and run events to the scoped run
store. Input-required wakes either wait condition; observation timeout does not
cancel work. Submit, full active-run listing, cancel, resume and exported output
remain `owner_unavailable` until Lane B supplies their owner APIs. Graph tick
persists its starting key before reporting that missing dispatch binding; graph
status can read the checkpoint. Aliases retain the facade's named-role
preference rather than fabricating an explicit model.

CLI `fabric host list`, hidden `fabric peer`, `fabric graph run/status/cancel/resume`, and
`fabric landing push/reconcile` bind in `src/`. CLI and MCP event reads lazily
pull bounded run-event pages into the home store. `unknown_hosts` reports
coverage gaps and `run_events_pending` reports another available page. Polling
holds no write transaction while waiting or contacting a peer. Host availability
and execution capability are separate observations.

The existing config installer already emits the explicit installed-binary
`fabric mcp --client <label>` registration arguments for all six harnesses;
Fabric consumes those arguments without another registration descriptor copy.
F9 cleanup removes the unused Rust subprocess, policy, lane-reader and execution
mirrors in the same change. Python deletion and shared skill/hook caller rewrites
remain with their authorised owners, outside this lane's write scope.

F10 adds native command-process tests for interpreter-free MCP negotiation and
mail, ingress refusal, exact mutation replay, graph starting checkpoints,
project-scoped run status/wait, and replay-safe lazy run-event ingestion. Git
fault tests use real temporary repositories and substitute a lost reply only
after the real push succeeds; the successor stays fenced beyond expiry and
remote reconciliation commits the receipt. Loaded local/worktree URL rewrites
are refused. These checks do not establish two-host or installed-candidate
acceptance: exact-head CI and the chair's authorised host smoke remain required.

Committed Git bundles and digest-checked chunk fetches have typed runtime owners.
Attaching transfer to a dispatched remote writer still belongs at Lane B's run
binding, where the writer lease and attempt result can be supplied together.
Fabric does not create a second writer registry or reconstruct a launch.


## Libraries and core records

Internal dependencies follow PORTING's table. Do not add `routing`, `quota`,
`telemetry` or `providers` directly to `fabric-runtime`: `run` supplies their
aggregate dispatch and capability APIs. Do not import `cli` into a library.
The executable composes CLI and MCP; `fabric-mcp` is a library.
Help and root resolution do not open SQLite or start threads.

| Crate | Proposed modules | Responsibility |
|---|---|---|
| `fabric-store` | `db`, `mail`, `tasks`, `claims`, `events`, `operations` | SQL schema, atomic transitions, typed rows; no Git, provider or SSH processes |
| `fabric-runtime` | `identity`, `service`, `runs`, `peer`, `git_transport`, `health` | Resolve project, enforce scope, compose store/run/supervisor, bound federation |
| `fabric-mcp` | `protocol`, `stdio`, `tools`, `render`, `registration` | MCP wire lifecycle, schemas, cancellation and compact rendering |
| `cli` Fabric module | `args`, `render` | Clap subtree and text/JSON presentation, calling the same service |

All records crossing a crate or persistence edge are typed serde models.
Facts built only at an edge derive `Serialize` only, never `Deserialize`. Shared identifiers
and process/authority types belong in `core` or `records`; private SQL rows stay in store. Fabric-only `Message`, `TaskState`, `MessageKind`, `TaskFence` and message/task refs live in `fabric-store`; `Page` and `Cursor` remain store types until a second owner needs them. New owned formats carry `version: 1`; refuse unsupported major
versions. SQL migrations are for future Rust versions only. Use integer epoch
milliseconds (`TimestampMs(i64)`) and checked durations, never JS float seconds.
Paths remain `PathBuf`/`OsString` internally; JSON paths must be UTF-8 or fail with
`PathEncoding`, rather than silently replacing bytes. SHA values are checked
`GitOid` values, not arbitrary strings. Sorted maps and an explicit canonical
encoder define request digests; no digest depends on hash-map iteration.

```rust
struct Identity {
    project: ProjectId, host: HostId, agent: AgentId,
    cwd: PathBuf, common_dir: Option<PathBuf>, client: ClientLabel,
}
struct Address { host: HostId, agent: AgentId }
struct Qualified<T> { host: HostId, id: T }
struct OperationKey { id: OperationId, digest: Digest }
struct Fence { id: ClaimId, generation: u64, token: SecretToken }
struct Message { id: MessageId, from: Address, to: Vec<Address>,
    kind: MessageKind, body: String, artifacts: Vec<ArtifactRef>,
    reply_to: Option<Qualified<MessageId>>, task: Option<Qualified<TaskId>> }
enum TaskState { Open, Active, Done, Cancelled }
enum Observed<T> { Current(T), Stale { value: T, observed_at: TimestampMs }, Unknown }
struct Page<T> { items: Vec<T>, next: Option<Cursor> }
struct OutputChunk { bytes: Vec<u8>, next_offset: u64, eof: bool, digest: Digest }
```

Message/task/run/operation IDs are opaque 128-bit values, hex encoded, generated
by the safe supervisor entropy API. HostId is generated once in new state;
ProjectId is a host-local identifier, recorded on every run. Runtime owns the
separate cross-host `ProjectKey` used for federation. AgentId is a bounded
inbox label, defaulting to the client label; an explicit agent label separates
concurrent inboxes; a shared label shares one read cursor, never competing claims.
Host IDs and display labels are separate. A route receipt comes
from `run`, copied exactly as `adapter/model@effort`; absence is explicit.
Client names, seats and model aliases never become invented route provenance.
Tokens are redacted from diagnostics; only the successful claimant receives one.

### Identity and configuration

One host-global state root defaults to `~/.local/share/provenant/v1`.
`--state-dir ABS` overrides `PROVENANT_STATE_DIR`, then the default; no XDG or
old-root fallback. Refuse a root inside an old root. Fabric owns `fabric/`
(`fabric.sqlite3`, `fetch/`, `locks/`); run owns top-level `runs/<RunId>/` and
its store. Config is the single user-owned `config.toml` under the root.
Candidate binaries always take an explicit isolated root.

Host-local ProjectId derives from HostId and canonical Git common directory;
linked worktrees share it. Non-Git directories use canonical cwd, local only.
Runtime derives a separate domain-separated cross-host ProjectKey from the
home-relative repository path and verifies it at each peer. Outside home,
remote placement is refused. Peers resolve that key to their own ProjectId;
no local ID is treated as a federation key. Moving a repository creates a new
project unless explicitly rebound in config. No scan or URL heuristic joins projects.

Config owns `FabricConfig { agent_label, client_label, home_hosts, peers, limits }`.
There is no profile layering, project overlay, profile mutation or per-knob env
input; `config show` and `check config` remain, with an optional `profile =` key.
Compiled presets and bundled prices/catalogue/adapter data ship as TOML assets.
The only product environment inputs are `PROVENANT_STATE_DIR` and
`PROVENANT_AGENT_LABEL`; provider and harness variables pass through as their
tools define them. `PROVENANT_AGENT_LABEL` replaces `AGENT_FABRIC_LABEL`.
No other `PROVENANT_*` or `AGENT_FABRIC_*` inputs are accepted. The installer
writes explicit `fabric mcp --client <label>` arguments. Project policy moves
from `fabric_policy.py` to typed config; confinement/run and CLI enforce it.
Invalid safety policy refuses launch. CLI owns `check` subjects config, install,
harness, release and `writing PATHS`, plus HookSpec (harness, protocol event,
matcher, timeout, handler). All areas register handlers as `hook <harness> <event>`;
installer consumes that table. Guard hooks deny unreadable or unbounded input.

Fabric defaults: body 64 KiB; input frame 1 MiB; JSON nesting 64; page 100 rows;
output tails 500 lines/64 KiB per read; result envelope 1 MiB; lease 1–3,600 s;
peer RPC deadline 30 s; Git bundle 64 MiB. Config may lower hard limits, never raise them.
The MCP composition applies lower input frame and JSON depth limits before
protocol dispatch through the existing bounded records decoder. Body, page,
lease and output requests are checked before owner calls. Oversized read
responses return `result_too_large`; committed mutation receipts retain the
transport hard ceiling so a configured read limit cannot hide their receipt.
Every wait takes an explicit budget: CLI up to 14400 s, MCP up to 55 s.
Run owns submission, concurrency, queue and active-attempt caps in
[r7-execution-run (#1356)](https://github.com/mblauberg/provenant/pull/1356).
Admission and operation insertion are atomic in run's store (PORTING R2/R6);
Fabric has no capacity ledger. Starting or uncertain attempts retain capacity
until reconciled; excess work is refused before acceptance.

## Store and state layout

One host-local SQLite database replaces the Fabric and host-operation databases.
Use bundled rusqlite, WAL, `foreign_keys=ON`, `synchronous=FULL`, busy timeout
at most 250 ms per attempt, capped by the remaining budget. Retry BUSY with backoff and
cancellation until the request deadline, then return `busy`; replay the whole
transaction only after rollback, with the same operation key. Never hold a lock
during backoff or turn an uncertain commit into a new mutation. Private root mode 0700, database and WAL/SHM
0600, created through supervisor safe filesystem APIs. Refuse symlinked state
components. No network filesystem SQLite and no shared connection across threads.
`Store` is a concrete connection owner, opened per worker; no generic backend
trait for a single database. Use numbered embedded migrations and `user_version`;
reject a future version before any mutation. First open serialises schema setup.

| Table | Key and important fields | Constraint or index |
|---|---|---|
| `projects` | `id`, common-dir key, home host | Unique canonical key; home changes require no active ownership |
| `agents` | `(project, host, agent)`, label, client, last_seen | Presence is observation, never process liveness |
| `messages` | `id`, project, sender, kind, body, artefact refs, reply/task refs, created | Immutable; references may be qualified external values |
| `deliveries` | `(message, recipient)`, project-local delivery `seq` | Unique delivery; index `(project, recipient, seq)` |
| `mail_cursors` | `(project, recipient)`, read-through `seq` | Monotonic max update; no claims or expiry |
| `tasks` | `id`, project, objective, state, owner, generation/token/expiry | State CHECK; CAS ownership; no task dependency table |
| `claims` | `id`, project, kind, scope, holder, generation/token/expiry, phase | Work scope rows canonicalised; one landing row per project/ref |
| `claim_paths` | `(claim, path)` | Prefix overlap checked under immediate transaction; `/a` does not overlap `/ab` |
| `operations` | `(project, id)`, kind, bounded frozen request, digest, phase, result | Same ID/different digest refused; prepared/sent/committed/unknown |
| `events` | autoincrement `seq`, project, kind, entity, payload, time, dedup key | Unique dedup key; `(project, seq)` cursor index |
| `run_cursors` | `(project, host)`, durable run cursor | Advance atomically with deduplicated event insertions |

All related mutation and event insertion commit in one transaction. A failed
event write rolls back the coordination mutation. No transaction stays open
while waiting, spawning a process, probing Git or making SSH calls. Generation
increments survive release and expiry; never delete/recreate the counter row.
Claims use the home host's clock only. Runtime waits use monotonic deadlines.
Run snapshots and terminal records remain `run`'s authority; Fabric stores IDs
and event cursors, not another mutable run status. On `events read` and `lanes --wait`,
runtime lazily pulls project-scoped `run::events(cursor)` into the home store. Persist
each per-host cursor with inserted events in one transaction, deduplicated by
`(host, run-event-id)`. The home pulls remote completions through the peer; remote
workers need no event push or local mail. No daemon is required: later reads
replay missed events. An unreachable peer yields `Unknown` with host identity
and leaves its cursor unchanged; partial results never claim complete coverage.

Mail `read --after seq` is non-mutating and returns ordered deliveries plus a
next cursor. `mark-read seq` monotonically advances only the authenticated
recipient's cursor, bounded by its highest delivered sequence. No ack or claim
token is issued. Task `finish` requires the active task fence; changing owner
requires release or expiry.

Work claims are advisory coordination, never authority to write a worktree;
`run` still obtains the actual supervisor writer lock.

### Fresh state and one-shot import

At cut-over, a one-shot read-only importer copies open and blocked Fabric tasks
and unacknowledged requests from the last 14 days, nothing else. Budget about
50 production lines for this bounded copy, not a general migration framework.
Open tasks stay Open; blocked tasks become Open with a blocker note. Retain
request correlation, recipients and task links within the imported subset;
references outside it remain explicit legacy references, never invented rows.
Each recipient's read cursor starts after the import; imported mail remains
available through an explicit `read --after` cursor.

Open the old store read-only and use one snapshot. Commit the imported rows,
cursor positions and a one-shot marker atomically in the new Fabric store;
a failed import leaves none of them, and a repeated cut-over skips a completed
import. No old-store writes, schema changes or fallback reads. Tests inject the
cut-over clock and isolate both roots, including requests at the 14-day boundary.
Do not import runs, sessions, process handles, claims, leases, graphs, config,
cooldowns, closed tasks, acknowledged mail or telemetry. Recreate peers and
labels through new config. Leave old state stores and transcripts untouched;
legacy harness-entry and instruction edits belong to the installer below.

## CLI and MCP surfaces

The binding table fixes command ownership; [cli.md (#1359)](https://github.com/mblauberg/provenant/pull/1359)
freezes exact flags and rendering. Requests support `--json`; MCP and peers use
protocol framing. Retryable mutations carry an operation ID. JSON uses CLI's
`schema_version`, `command`, `status`, `data`, `error` envelope with a 1 MiB cap,
pagination and stable error codes; Fabric does not define another CLI envelope.
No old aliases remain. CLI prompt files are read by confinement and become typed
prompt bytes before service invocation; MCP accepts text and typed artefact refs.

| `provenant fabric …` | Operations |
|---|---|
| `identity` | Project, worktree, host and inbox address |
| `mail send\|read\|mark-read` | Explicit recipient(s), `--after seq`, per-recipient cursor |
| `task create\|list\|claim\|update\|finish\|release` | Objective and lifecycle; update cannot bypass ownership |
| `events read\|note` | Cursor, limit, optional wait and append-only notes |
| `claim acquire\|renew\|verify\|release\|list` | Advisory issue/path work ownership |
| `landing acquire\|renew\|verify\|release\|wait\|push\|reconcile` | Branch/expected SHA/fence; push stays an explicit authorised CLI action |
| `host list` | Read-only peer and adapter readiness observations |
| `mcp` | stdio server mode of this executable |
| hidden `peer` | Fixed framed peer ingress; no user command passthrough |

Run owns top-level `dispatch`, `lanes [--wait]`, `output`, `cancel`, `resume`
and `reconcile`. Fabric keeps messaging, tasks, agent identity/label, peer hosts,
MCP and the coordination operations above. There is no Fabric run subtree or
Fabric-level run reconciliation; supervisor adds no run verbs. Runtime routes
federated queries and controls to run on the owning host.

`dispatch` uses one `Submission { tasks: Vec<TaskRequest>, concurrency }`;
no scalar/list union. A single task is a one-item list. Resume is a separate
`ResumeRequest { target: AttemptRef, prompt, expected_generation }`; it cannot
change route, account or authority.

Expose 11 tools instead of today's 20. Each tool's tagged input enum rejects
unknown fields; generate its checked JSON schema from an explicit shared field
catalogue without another schema crate. Snapshot/schema tests bind the catalogue
to serde acceptance. No `detail`, scalar/list aliases or numeric-string coercion.

All inputs use `op` to select one typed operation.

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

For `fabric_graph`, `run` and `resume` take a project-local graph document path;
`status` and `cancel` take its `id`. The document declares each node's `access`
as `read_only` or `worktree_write`; a writer also names its absolute `worktree`.
The run owner must enforce those permissions before launching a provider.
A graph attempt may use its next route after an auth refusal, usage limit or
unavailable adapter when no provider launched. After launch, only read-only
nodes may fall back. A failed writer retains its worktree in the checkpoint for
the chair. Missing launch evidence counts as launched. Graph attempts use
`limits.graph_timeout_ms` from the Fabric configuration.

Cancellation persists the admission fence before asking the run owner to cancel
accepted attempts. An unavailable owner returns its typed error; the fence still
prevents new graph work. Resume checks the same document digest and reuses each
attempt key. Until the dispatch/control owner is bound, execution returns
`owner_unavailable`; graph status and the admission fence remain available.

Responses contain typed `structuredContent` and a short text digest: IDs, state,
exact route if known, and next cursor/artefact references. Do not repeat result
tails in every status call. Output bytes are UTF-8 text when valid; otherwise an
explicit byte-array encoding, never lossy text. Fetch copies only run-exported
regular files, refuses symlinks/traversal and enforces total byte/file caps. It
verifies a manifest digest before atomic publication below `fetch/<host>/<run>/`.
Output tools accept no arbitrary source or destination filesystem path. Graph
document paths resolve within the authenticated working directory.

CLI `lanes --wait` uses `WaitCondition { Any, All }`: Any returns on a
terminal target; All returns when all selected targets are terminal; either
returns immediately on any `InputRequired`. Freeze the target set at invocation;
CLI owns exact selector and condition flags. Every wait requires an explicit
budget: CLI up to 14400 s, MCP up to 55 s. A met condition exits 0, including
`InputRequired` or a failed/cancelled target; target outcomes remain data.
Landing `wait` observes a supplied generation and returns on release or holder/
generation change; it never acquires the lease. Apply the same CLI/MCP bounds.
Long CLI waits loop bounded owner/peer reads under one monotonic deadline,
refreshing events between reads; a peer RPC never holds the full CLI budget.

### MCP protocol and SDK decision

Recommend an in-house, bounded stdio JSON-RPC transport and **no rmcp amendment**.
The official [rmcp manifest](https://github.com/modelcontextprotocol/rust-sdk/blob/rmcp-v3.7.0/crates/rmcp/Cargo.toml)
for 3.7.0 requires `tokio`, `tokio-util`, `futures` and `tracing` even with optional features disabled.
Using it would require both a dependency amendment and reversal of no-async.
Its maintained protocol types and lifecycle machinery are valuable, but the
single stdio tools-only server does not justify that runtime expansion. Revisit
if HTTP transport, elicitation or subscriptions become actual requirements.
The original budget was 700–1,000 production lines for framing, validation,
lifecycle and schemas. Both chairs accepted the overrun with no rmcp amendment:
the `fabric-mcp` crate owns **1,438 production Rust lines**, and the typed bound
checks in `fabric-runtime/src/coordination/input.rs` are **440 shared runtime
lines**, counted separately from MCP. Counts include physical lines, blanks and
comments before each file's `#[cfg(test)]`, excluding separate test files and
fixtures. The framing/lifecycle/schema subtotal is 1,106 lines; with shared
validation it is 1,546, exceeding the original upper bound by 546 lines.
The schema catalogue stays explicit, with its serde agreement test, because
deriving schemas from the typed params would require a new proc-macro crate.

Support `2025-11-25` and `2026-07-28`; do not add `2025-06-18` to the supported
versions. Verbatim first-request stdio fixtures captured on 24 October 2026
provide shipped-client evidence for each era:
[Claude Code 2.1.309](../../../../tools/front-door/crates/fabric-mcp/tests/fixtures/claude-code-2.1.309-discover.json)
negotiates `2026-07-28` through `server/discover`, with an
[`initialize` fallback](../../../../tools/front-door/crates/fabric-mcp/tests/fixtures/claude-code-2.1.309-initialize.json).
[Codex 0.159.0](../../../../tools/front-door/crates/fabric-mcp/tests/fixtures/codex-0.159.0-initialize.json)
sends only `initialize`, requesting `2025-06-18`, and receives the supported
`2025-11-25` version through legacy negotiation. Both eras therefore serve a
shipped client; transport tests assert each fixture's reply.
Both use UTF-8 newline JSON-RPC, protocol-only stdout and diagnostics on stderr.
One negotiated-version enum (`Era`) at the framing edge selects legacy on
`initialize` or modern on versioned `_meta`, including a `server/discover`
probe; one shared request handler applies its era guards and refuses switches.

| Profile | Required wire behaviour |
|---|---|
| [2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle) | `initialize`, `notifications/initialized`, negotiated version/capabilities and `ping`; `tools/list`, `tools/call`, `notifications/cancelled` and EOF |
| [2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning) | No `initialize`, `notifications/initialized` or `ping`; require per-request `_meta` keys `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities`; implement `server/discover` |

Modern results require `resultType`; ordinary Fabric results use `"complete"`.
Unsupported versions return `UnsupportedProtocolVersionError` with supported versions.
Modern `tools/list` requires `ttlMs` and `cacheScope`; use a fixed catalogue
and deterministic order, which upstream recommends (SHOULD). See the published
[tools contract](https://modelcontextprotocol.io/specification/2026-07-28/server/tools).
No resources, prompts, sampling, subscriptions or task extension is advertised.
Keep JSON-RPC errors distinct from tool execution failures (`isError: true`);
preserve string/integer IDs. Cancellation suppresses late responses per the
[stdio rules](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio),
but never implicitly cancels a launched run; use explicit `cancel`.

## Concurrency, custody and recovery

No Fabric daemon, listener, async executor or in-memory authoritative registry.
One MCP process serves one client. A bounded stdin reader and one stdout writer
surround a pool of four request workers with queue capacity 32; cancellation and
protocol control use a separate bounded control path so long waits cannot block
them. Saturation returns a typed busy error. Workers own their connections.
Framing reads at most the cap plus one byte before refusal; no unbounded
`read_line`. Duplicate live request IDs are refused. The writer serialises whole
frames with a 30 s write deadline; a wedged reader ends this transport. On EOF
or a read-side error, stop admission and drain accepted work for at most 55 s,
writing its replies while stdout is viable; then cancel cooperative observations
and join within a further 1 s. A write-side error stops all further writes;
detached owners survive. Stuck transport children are stopped only through their
supervisor-owned handles. An unreadable stdout ends the MCP transport without
signalling provider runs. Library code starts no surprise global threads.

Run owners are detached modes of the same executable, created through `run` and
supervisor custody. Before acknowledgement, run durably stores the bounded frozen
request, resolved route/access, operation digest and preallocated attempt IDs.
An accepted receipt means durable admission, not proof that a provider started.
`run::recover` replays an accepted operation after a pre-spawn crash. A per-operation
OS launch lock passes from launcher to detached owner and stays held before any
provider starts. The owner publishes its identity before provider launch. Recovery
must acquire that lock and prove the previous owner and provider absent/dead.
The owner persists provider-launch intent before spawning: after that boundary,
recovery observes or reports unknown and never replays the attempt. Only accepted
work with no provider-launch intent may resume startup. A held lock, live provider
or uncertain identity means starting/unknown, never a second provider launch. For intent-without-start uncertainty, explicit `reconcile` may abandon the
attempt as `OutcomeUnknown` only after supervisor proves no owned process remains;
retain evidence and release capacity, never infer whether work ran. Any retry is
a new authorised operation. Without absence proof it remains unresolved.
Cancellation can durably close an unstarted operation and prevents recovery from spawning it. Test every acceptance/spawn/
identity-publication boundary, including a lost launch reply. Fabric never stores a bare PID as a
cancellation capability. Status comes from `run::snapshot`, retaining separate
owner state, provider state and publication state. A live provider with a dead
launcher is not terminal. Missing or malformed evidence is unknown, not success.
Silence, MCP disconnect, bookkeeping failure and ENOSPC never authorise killing a
healthy provider. Only explicit cancellation, an authorised timeout or the run
owner's established custody rules can stop it.

### Peers and Git

Each configured peer is the same user's trusted host, accessed through system
SSH, no listener. Supervisor captures bounded stdin/stdout/stderr using a fixed
argv and deadline. The forced remote command is `provenant fabric peer`; it
accepts one versioned request, not a command string. SSH destination comes only
from validated config. Provider credentials stay on the executing host.
Handshake verifies protocol major, host ID and cross-host ProjectKey. Version mismatch
refuses writes and reports incompatible reads. Peer fan-out uses at most
four transport threads, never holds a DB transaction, and never recursively fans
out from a peer request. Control cancellation stops the SSH child, not its remote
run. Remote owner status is authoritative; unreachable peers return `Unknown`,
with no persisted peer snapshot cache.

All project coordination routes to its configured home, default local. Messages,
tasks, events, claims and landing share that authority; a non-home host does no
local mirror writes. An unreachable home returns `HomeUnavailable`, including
for mailbox send. Local run execution remains available. Recipient HostId identifies an inbox address, not its storage location: send,
read and mark-read all target the project home, including remote recipients.
No pending message queue, distributed task claim, merged clock ordering or
home-host auto-election.
With the current laptop home, workshop coordination requires SSH back to the
laptop, Remote Login enabled there, and the laptop awake and reachable.
Decided by the chairs (R11 feature scope): remote workers return results only
through runs, with no remote mail or tasks; the home chair reads and pulls their
results and events. The host transport sits behind one trait, so remote mail
could later be added as one module without a second consistency model. The current config defaults both projects to laptop; 786 distinct
workshop run IDs show that this dependency matters.
Home reassignment refuses active ownership and requires an explicit new-state
transfer decision. This is the main federation scope reduction to approve.

Remote mutations use `(OperationId, request digest)`. Persist prepared, mark sent
before transport, and query the owning host on uncertainty. Same ID/different
body is a conflict. No fallback after a request might have crossed the boundary.
The peer persists operation acceptance before spawning a run; the run owner's
own idempotency key closes the database/process gap. Read timeouts return
`Unknown`; neither reads nor writes use a peer snapshot cache.

Writer lanes receive a committed base through a bounded Git bundle. Retain
`refs/provenant/transport/<operation>` isolation, validated ref names, hardened
Git environment, no hooks, no replace objects and no remote shell Git invocation.
Receiver checks advertised object identity, bundle size, worktree cleanliness
and writer lease before launch. Result binds base, head, host, run and worktree.
The chair verifies the fetched head against that result before any local landing.
Uncommitted changes are not transported. Retention of temporary refs and bundles
is explicit new-state maintenance; it never touches old run directories.

Landing runs entirely on the home host under a per-ref OS lock. Acquire a
persistent generation/token bound to expected remote OID and candidate OID.
Before pushing, atomically mark the operation `pushing`; expiry cannot permit a
successor while it is unresolved. Validate actual remote OID and use Git's exact
`--force-with-lease=<ref>:<expected>` fence, after proving the candidate is a
fast-forward. This is not authority for a non-fast-forward rewrite. Never hold
a SQLite write lock across Git. On lost response or owner death, keep the lease
blocked and reconcile the remote OID: candidate means committed; expected means
retry may be explicitly requested; any other value means conflict. An absent
proof remains unknown and cannot release the fence automatically. Renewal,
release and push all compare holder, generation and token. A TTL alone cannot
fence an external Git write.

## Cross-chair interfaces to freeze

These are requested signatures, not claims that the inert crates implement them.
`Result<T, E>` uses the owning crate's typed error. Shared structs below belong
in core/records as indicated. Fabric must not grow private copies while waiting.
The other chair may choose names, but must preserve the ownership and semantics.

The principal shared request/result types are concrete serde records in
`records`; numeric wrappers below are checked at construction. Extend these
through the shared freeze, rather than adding per-edge JSON bags:

```rust
struct Submission { tasks: Vec<TaskRequest>, concurrency: u16 }
struct TaskRequest { id: RequestId, prompt: InputBytes, inputs: Vec<ArtifactRef>,
    route: RouteRequest, access: Authority, cwd: PathBuf, timeout: DurationMs,
    delivery_id: Option<DeliveryId> }
struct SubmissionReceipt { runs: Vec<AttemptRef>, routes: Vec<RouteReceipt> }
struct ResumeRequest { target: AttemptRef, prompt: InputBytes, expected_generation: u64 }
enum ReconcileDecision { AbandonUncertain }
struct ExecutionContext { project: ProjectId, actor: Address, authority: Authority }
struct AttemptTempPaths { root: PathBuf, provider_tmp: PathBuf, browser_socket: Option<PathBuf> }
struct RunQuery { project: ProjectId, ids: Vec<AttemptRef>,
    states: Vec<RunState>, cursor: Option<Cursor>, limit: u16, condition: WaitCondition }
enum WaitCondition { Any, All }
struct RunSnapshot { project: ProjectId, target: AttemptRef, state: RunState, owner: OwnerObservation,
    provider: ProviderObservation, publication: PublicationState,
    route: Option<RouteReceipt>, artifacts: Vec<ArtifactRef>, observed_at: TimestampMs }
struct RegistrationDescriptor { executable: PathBuf, args: Vec<String>,
    env: BTreeMap<String, String> }
```

`Observed`, `OutputChunk`, `OperationKey` and `ResumeRequest` also live in
records, so run never imports Fabric. `WaitBudget { deadline: MonotonicDeadline }`
and cooperative `CancelToken` live in core; supervisor consumes them. Runtime's
peer client takes `PeerRequest { version, target_host, project: ProjectKey, operation, action }`;
`action` is a closed enum for coordination, run, health and Git transfer. There
is no arbitrary executable field. The peer resolves ProjectKey to its local
ProjectId. The target is the execution host for runs and the project home for coordination.
Fabric derives `ExecutionContext.project` from Identity.project; Submission
carries no duplicate project. Run checks context project/actor/authority, reloads
project policy and refuses broader access. Snapshots must match project scope.
Host health includes run's typed
`ExecutionCapabilities`. Run also supplies
`temporary_paths(ProjectId, AttemptRef) -> Result<AttemptTempPaths, RunError>`
for host-local confinement/socket checks; it replaces Python host-helper imports.
Run supplies these record values to confinement, which never imports run.
Bounded stdout writing requires supervisor's safe
`write_frame(&StreamHandle, &[u8], WaitBudget) -> Result<(), IoError>`; plain
blocking `write_all` cannot promise bounded EOF shutdown under backpressure.

Fabric supplies `FabricService::open(ResolvedConfig, Identity)`, typed methods for
the 10 tool domains. `fabric-mcp::registration(executable: &Path, client: ClientLabel) ->
RegistrationDescriptor` owns MCP registration; config supplies managed-file
reconciliation and preserves unrelated client entries (PORTING R2).
The installer exposes `install [--dry-run]`, `check install` and `uninstall`, with
`--legacy-checkout ABS`; re-applying install recovers partial state. Cut-over
rewrites legacy MCP entries, replaces/removes legacy hook, skill, workflow and
instruction links, and updates `~/.agents/AGENTS.md` and linked files for the label rename. Match a compiled legacy-entry list by
exact command/link target under that checkout, read links with `readlink` only,
and print each harness-config change before applying it. Rollback reinstalls
`pre-rust-rewrite`; import never modifies the old store. `fabric-store::Store::open(StateRoot)` and transaction
methods take a `Request` with an injected clock; the store derives decision times and lease expiries from that clock.
`CancelToken`, `WaitBudget`, `CommandSpec` and filesystem handles are safe
supervisor/core types. The CLI and MCP own no filesystem policy.
If a signature requires an extra allowed dependency edge, settle the shared
layering table before implementation; do not introduce a hidden edge or `pending`.

## Errors and diagnostics

Libraries use thiserror enums with typed sources and relevant IDs/paths, without
raw secret values. Expected contention, stale fences and uncertainty are data.
Common service codes: `invalid_input`, `not_found`, `conflict`, `busy`,
`stale_fence`, `home_unavailable`, `peer_version`, `launch_unknown`,
`continuation_unsupported`, `confinement_refused`, `worktree_missing`, `output_too_large`, `state_corrupt`,
`publication_failed`, `cancelled`, `timeout`, `io`. Return retry advice only when safe.
Include `operation_id` on uncertain mutations. Use CLI's exits: 0 success,
1 failed/refused/findings, 2 invalid input/version, 3 cannot proceed (setup,
I/O or contention), 124 wait deadline, 130 invocation cancelled. There is no
exit 4: partial coverage, unknown targets and input-required are typed `data`.
A wait whose condition is met exits 0 regardless of target outcome; it never
cancels the underlying run on deadline. Missing evidence needed to proceed is
3. MCP budget expiry returns typed timeout data and preserves JSON-RPC errors
separately from domain tool errors. Diagnostics go to stderr, with no panics,
production unwraps or swallowed failed writes. Health checks do not repair,
prune or acquire coordination claims.

## Reuse and deliberate stripping

| Material | Keep | Strip or rewrite |
|---|---|---|
| Existing `fabric-store` plus #1332 | Immediate transactions, token/fence checks, overlap tests, contention classification, atomic message+event writes, checked filesystem opening and push critical-section idea | Included TS schema, compatibility migrations, JS truthiness/number/date spelling, legacy terminal-row mapper, backend trait for one store, all oracle/differential machinery |
| Existing `fabric-runtime` plus #1334 | Git redirect scrubbing, common-dir identity, bounded inputs, route/access validation invariants, peer version checks, Git object/ref validation | Python owner argv builders, `.agent-run`/product-root/seat discovery, status dictionaries, `Value` field helpers, JS semantics in `pending`, subprocess JSON shims, duplicated pool/routing logic |
| #1344 workflow | Bounded hook envelope parsing, typed authority validation, conservative reinjection and disclosure checks where workflow still owns them | Do not move receipt/session/review schemas into Fabric; no copied Python globals or oracle processes |
| Existing supervisor/run/records | Safe process identity, custody and publication, writer lock, shared codecs and run records after interface review | Fabric-specific `ps` wrappers and speculative alternate run readers once run owns snapshots |
| Empty `fabric-mcp` crate | Workspace slot and layering | Design implementation from the supported MCP profiles; there is no server code to preserve |

Tests in these PRs supply scenarios, not expected old-runtime outputs. Re-express
invariants as new Rust-native tests. Do not cherry-pick their parity documents or
portable compatibility helpers merely to claim lines reused.

## Verification

No new dependency is needed. Use `cargo test`, std test harness, `tempfile`,
`rusqlite` and checked-in `.json`/`.txt` fixtures. The in-house snapshot helper
compares output against a checked-in file, rewrites only through an explicit
test-helper argument, and prints a line diff on mismatch. Fixtures
inject IDs and clocks, so no redactions are needed. Budget 60–100 lines; revisit
only beyond about 150 lines or if inline snapshots become valuable. No insta, proptest, loom or cargo-nextest
amendment is proposed. A separate property-testing dependency can be reconsidered
if seeded deterministic interleaving tests prove insufficient.

| Layer | Rust-native tests and fault cases |
|---|---|
| Store unit | Mail cursor isolation, monotonic mark-read and replay, task owner CAS, path overlap boundaries, generation monotonicity, event rollback, schema future-version refusal, UTF-8/number limits |
| Process contention | Spawn the same test executable as child workers, barrier via pipes; race cold opens, task/work claims and landing holders; exactly one winner, no timing-only sleeps |
| State faults | Supervisor seams inject publication write/fsync/rename faults only; bundled SQLite owns its I/O. Force SQLITE_FULL with `PRAGMA max_page_count` and growth inserts; force BUSY with a second connection holding RESERVED (`BEGIN IMMEDIATE`); verify bounded retries and reopen after owner death |
| Run facade | Fake run owner with durable operation IDs; lost launch reply, dead launcher/live provider, corrupt receipt, cancellation during publication; no duplicate launch or healthy-provider signal |
| MCP | Duplex pipes to the built binary; both protocol eras, first-message selection, `server/discover`, required modern metadata/result/cache fields, capabilities, `tools/list` schemas, strict calls, parse/oversize/depth errors, string/integer IDs, cancellation, full queue, EOF and broken stdout |
| CLI snapshots | Help side-effect free; top-level run verbs, Fabric subtree, CLI envelope/1 MiB cap, exits without 4, partial/input-required data; met waits exit 0, CLI 14400 s/MCP 55 s boundaries; no Python/Node in PATH |
| Federation | Fake SSH executable, protocol mismatch, skewed clocks, unreachable home, delayed/lost response, idempotent launch reconciliation and lazy run-event replay/cursor atomicity; owning-host liveness only |
| Git and fetch | Local temporary repos and forced peer command; dirty writer refusal, wrong OID/ref, bundle cap, symlink traversal, digest mismatch, moved remote, push reply loss and blocked unresolved fence |
| Import/install | Open/blocked tasks, unacked 14-day requests, cursor positions, atomic one-shot retry, old-store immutability; six-client MCP rewrite, label rename in global instructions/linked files, legacy exact matching, dry-run, re-install recovery and unrelated config preservation |

Run only fast touched-crate tests and static checks locally. CI owns full suites,
Linux and macOS custody/concurrency tests and exact-head install integration.
Candidate tests use isolated new roots and fake providers. Two-real-host smoke
and live-provider trials require their own authorised run; this design does not
claim they occurred. All acceptance is against the new design and surviving
external protocols. Delete old tests with their callers, even while main is red.

## Deletion inventory and line budget

Every row below names a whole-file deletion owned by this subsystem. Counts use
`len(bytes.splitlines())` over Git-tracked files at the baseline, including blanks
and final unterminated lines. Directory prefixes shorten paths without globs.
No source is deleted by this documentation PR. Shared-owner exclusions follow
the table; they are not silently included in the net savings.

The CLI parser step retired `scripts/configure-fabric-mcp.py` (818 actual
lines) and the remaining `tests/test_configure_fabric_mcp.py` (1,148 actual
lines) with the implicit no-argument MCP route. The inventories below retain
the original planning baseline; do not count those deletions again in later
owner slices. The explicit transport and new registrations land at cut-over.

| File | Lines |
|---|---:|
| `runtime/fabric/README.md` | 752 |
| `runtime/fabric/bin/fabric` | 62 |
| `runtime/fabric/bin/fabric-mcp` | 63 |
| `runtime/fabric/live-sandbox-smoke.py` | 215 |
| `runtime/fabric/mcp-smoke.mjs` | 12 |
| `runtime/fabric/package-install-smoke.mjs` | 111 |
| `runtime/fabric/package.json` | 31 |
| `runtime/fabric/schema.sql` | 240 |
| `runtime/fabric/smoke.ts` | 62 |
| `runtime/fabric/src/batch-execution.ts` | 337 |
| `runtime/fabric/src/catalogue.ts` | 281 |
| `runtime/fabric/src/cli.ts` | 912 |
| `runtime/fabric/src/execution-input.ts` | 610 |
| `runtime/fabric/src/execution.ts` | 895 |
| `runtime/fabric/src/export-tool-schemas.ts` | 5 |
| `runtime/fabric/src/graph/document.ts` | 526 |
| `runtime/fabric/src/graph/gates.ts` | 80 |
| `runtime/fabric/src/graph/owner.ts` | 554 |
| `runtime/fabric/src/graph/store.ts` | 356 |
| `runtime/fabric/src/graph/surface.ts` | 149 |
| `runtime/fabric/src/hosts.ts` | 92 |
| `runtime/fabric/src/identity.ts` | 196 |
| `runtime/fabric/src/json-subprocess.ts` | 107 |
| `runtime/fabric/src/landing-cli.ts` | 175 |
| `runtime/fabric/src/lane-wait.ts` | 110 |
| `runtime/fabric/src/lane-watch.ts` | 56 |
| `runtime/fabric/src/owner-output.ts` | 350 |
| `runtime/fabric/src/peer-lane-rows.ts` | 17 |
| `runtime/fabric/src/peer-lanes.ts` | 77 |
| `runtime/fabric/src/pools.ts` | 210 |
| `runtime/fabric/src/process-identity.ts` | 392 |
| `runtime/fabric/src/ps.d.mts` | 1 |
| `runtime/fabric/src/ps.mjs` | 20 |
| `runtime/fabric/src/python-owner.ts` | 49 |
| `runtime/fabric/src/resume.ts` | 349 |
| `runtime/fabric/src/run-reader.ts` | 241 |
| `runtime/fabric/src/run-registry.ts` | 283 |
| `runtime/fabric/src/run-root.ts` | 96 |
| `runtime/fabric/src/run-status.ts` | 770 |
| `runtime/fabric/src/seat-index.ts` | 120 |
| `runtime/fabric/src/seat-lanes.ts` | 134 |
| `runtime/fabric/src/server.ts` | 678 |
| `runtime/fabric/src/sessions.ts` | 243 |
| `runtime/fabric/src/store.ts` | 1,314 |
| `runtime/fabric/src/success-status.ts` | 8 |
| `runtime/fabric/src/surface.ts` | 213 |
| `runtime/fabric/src/tool-schemas.ts` | 101 |
| `runtime/fabric/tests/adapter-registry.test.ts` | 233 |
| `runtime/fabric/tests/atomic-write.d.mts` | 1 |
| `runtime/fabric/tests/atomic-write.mjs` | 10 |
| `runtime/fabric/tests/batch-host-worker.ts` | 18 |
| `runtime/fabric/tests/catalogue.test.ts` | 151 |
| `runtime/fabric/tests/claim-contention-worker.ts` | 19 |
| `runtime/fabric/tests/claim-worker.ts` | 29 |
| `runtime/fabric/tests/cold-start-worker.ts` | 30 |
| `runtime/fabric/tests/concurrency-worker.ts` | 59 |
| `runtime/fabric/tests/dispatch-host-worker.ts` | 20 |
| `runtime/fabric/tests/execution-lifecycle.test.ts` | 2,555 |
| `runtime/fabric/tests/execution-owner-fixture.mjs` | 376 |
| `runtime/fabric/tests/execution-pruning.test.ts` | 375 |
| `runtime/fabric/tests/fabric.test.ts` | 2,017 |
| `runtime/fabric/tests/fixture-processes.test.ts` | 74 |
| `runtime/fabric/tests/fixture-run-root.test.ts` | 90 |
| `runtime/fabric/tests/fixtures/attempt.json` | 62 |
| `runtime/fabric/tests/fixtures/graph-live-free.json` | 52 |
| `runtime/fabric/tests/fixtures/status.json` | 129 |
| `runtime/fabric/tests/graph-document.test.ts` | 171 |
| `runtime/fabric/tests/graph-gate-worker.ts` | 14 |
| `runtime/fabric/tests/graph-gates.test.ts` | 111 |
| `runtime/fabric/tests/graph-runner.test.ts` | 549 |
| `runtime/fabric/tests/helpers/fixture-processes.ts` | 68 |
| `runtime/fabric/tests/helpers/temp-root.ts` | 36 |
| `runtime/fabric/tests/hermetic-tmpdir.ts` | 30 |
| `runtime/fabric/tests/host-liveness.test.ts` | 92 |
| `runtime/fabric/tests/hosts.test.ts` | 202 |
| `runtime/fabric/tests/json-subprocess.test.ts` | 55 |
| `runtime/fabric/tests/landing-cli.test.ts` | 209 |
| `runtime/fabric/tests/lane-reader.test.ts` | 212 |
| `runtime/fabric/tests/lane-wait.test.ts` | 254 |
| `runtime/fabric/tests/lane-watch.test.ts` | 68 |
| `runtime/fabric/tests/lanes-cli.test.ts` | 259 |
| `runtime/fabric/tests/lean-surface.test.ts` | 368 |
| `runtime/fabric/tests/lifecycle-owner-fixture.mjs` | 266 |
| `runtime/fabric/tests/named-sessions.test.ts` | 677 |
| `runtime/fabric/tests/namespace-worker.ts` | 33 |
| `runtime/fabric/tests/nested-dispatch.test.ts` | 47 |
| `runtime/fabric/tests/peer-lane-rows.test.ts` | 42 |
| `runtime/fabric/tests/process-reaping.test.ts` | 278 |
| `runtime/fabric/tests/project-identity.test.ts` | 328 |
| `runtime/fabric/tests/run-closure.test.ts` | 43 |
| `runtime/fabric/tests/run-reader-events.test.ts` | 799 |
| `runtime/fabric/tests/seat-index.test.ts` | 15 |
| `runtime/fabric/tests/spawn-fault-preload.mjs` | 24 |
| `runtime/fabric/tests/surface.test.ts` | 1,012 |
| `runtime/fabric/tests/task-claim-worker.ts` | 34 |
| `runtime/fabric/tests/temp-root.test.ts` | 129 |
| `runtime/fabric/tests/tool-budget.mjs` | 26 |
| `runtime/fabric/tests/v2-owner-fixture.mjs` | 234 |
| `runtime/fabric/tsconfig.json` | 14 |
| `runtime/fabric/vitest.config.ts` | 3 |
| `scripts/configure-fabric-mcp.py` | 826 |
| `scripts/fabric-hosts` | 14 |
| `scripts/fabric_git.py` | 1,210 |
| `scripts/fabric_hosts.py` | 807 |
| `scripts/fabric_lanes.py` | 965 |
| `tests/fixtures/fabric-v1/lane-d-tool-params.json` | 17 |
| `tests/golden/cases/fabric/fabric-case-01.json` | 12 |
| `tests/golden/cases/fabric/fabric-case-02.json` | 12 |
| `tests/golden/cases/fabric/fabric-case-03.json` | 13 |
| `tests/golden/cases/fabric/fabric-dispatch-invalid-option.json` | 13 |
| `tests/golden/cases/fabric/fabric-dispatch-list.json` | 14 |
| `tests/golden/cases/hosts/hosts-case-01.json` | 12 |
| `tests/golden/cases/hosts/hosts-case-02.json` | 12 |
| `tests/golden/cases/lease/lease-case-01.json` | 12 |
| `tests/golden/cases/lease/lease-case-02.json` | 12 |
| `tests/golden/cases/peer/peer-alias-01.json` | 12 |
| `tests/mcp_judge/README.md` | 65 |
| `tests/mcp_judge/client.py` | 260 |
| `tests/mcp_judge/conftest.py` | 8 |
| `tests/mcp_judge/test_transcripts.py` | 125 |
| `tests/mcp_judge/transcripts/core.jsonl` | 45 |
| `tests/mcp_judge/transcripts/core.scenario.jsonl` | 45 |
| `tests/mcp_judge/transcripts/fetch.jsonl` | 2 |
| `tests/mcp_judge/transcripts/fetch.scenario.jsonl` | 2 |
| `tests/mcp_judge/transcripts/protocol.jsonl` | 2 |
| `tests/test_configure_fabric_mcp.py` | 1,310 |
| `tests/test_fabric_hosts.py` | 3,861 |
| `tests/test_fabric_node_discovery.py` | 83 |
| `tests/test_fabric_skill_params.py` | 78 |
| `tests/test_fabric_v1_contract.py` | 123 |
| `tests/test_live_sandbox_smoke.py` | 124 |
| `tools/front-door/crates/fabric-store/tests/oracle.mjs` | 162 |
| `tests/golden/cases/lanes/lanes-case-01.json` | 12 |
| `tests/golden/cases/lanes/lanes-case-02.json` | 12 |
| `tools/front-door/src/help/fabric.txt` | 5 |
| `tools/front-door/src/help/hosts.txt` | 10 |
| `tools/front-door/src/help/lanes.txt` | 9 |
| `tools/front-door/crates/supervisor/src/fabric_ps.rs` | 367 |
| `tools/front-door/crates/fabric-runtime/PORT_STATUS.md` | 133 |
| `tools/front-door/crates/fabric-store/PORT_STATUS.md` | 80 |
| `docs/specs/fabric-v2.md` | 387 |
| `docs/specs/fabric-hosts.md` | 292 |
| `skills/orchestrate/references/fabric-identity.md` | 31 |

Whole-file total: **143 files, 37,253 lines**. This includes all
tracked files below `runtime/fabric/`, the old MCP judge and the named external
Fabric helpers/tests. The root Node manifests are deliberately excluded.

Shared deletions requiring another owner or caller closure, excluded above:

| File | Lines | Disposition |
|---|---:|---|
| `package-lock.json` | 3,287 | Shared npm/Ajv owner removes after utility-schema callers move; Fabric workspace entry goes immediately |
| `package.json` | 26 | Shared npm/Ajv owner removes after utility-schema callers move; Fabric workspace entry goes immediately |
| `scripts/lib/agent-fabric-tsx-loader.sh` | 166 | Shared Node support owner deletes after non-Fabric consumers move |
| `scripts/node-workspace-preflight.mjs` | 119 | Shared Node support owner deletes after non-Fabric consumers move |
| `scripts/node-workspace-provision.py` | 130 | Shared Node support owner deletes after non-Fabric consumers move |
| `scripts/retry-npm-install.py` | 86 | Shared Node support owner deletes after non-Fabric consumers move |
| `skills/orchestrate/scripts/fabric_policy.py` | 100 | Config/confinement and lint owners absorb policy; delete after those callers land |
| `tests/test_fabric_records.py` | 816 | Records/routing owner retains or replaces account/cooldown tests; not Fabric deletion |
| `tests/test_node_workspace_preflight.py` | 173 | Shared Node support owner deletes after non-Fabric consumers move |
| `tests/test_node_workspace_provision.py` | 207 | Shared Node support owner deletes after non-Fabric consumers move |
| `tests/test_npm_attestation_negative.py` | 276 | Shared Node support owner deletes after non-Fabric consumers move |
| `tests/test_retry_npm_install.py` | 80 | Shared Node support owner deletes after non-Fabric consumers move |

`skills/orchestrate/scripts/fabric_records.py` (647 lines) is also a shared
records/routing deletion, not a Fabric store module. Its name alone grants no
ownership. The remaining `tests/fixtures/fabric-v1/` records are shared with run,
records and quota; their owners remove them with their callers. Only Fabric's
old tool-parameter fixture is included above; shared fixtures are excluded.

Existing inert Rust at the baseline is 5,686 lines across 26 `.rs` files,
including tests. The replacement budget after cutting sessions/graphs is 10,000–14,000 Rust production/test
lines across the three crates, plus 1,000–2,000 lines in the CLI/config edges, including the small importer.
That target includes retained Rust, not only newly authored lines. Removing the
old files and replacing the existing Rust gives approximately 26,939–31,939 lines of
net reduction. These are design estimates, not measured delivered savings;
shared Node manifests/policy/tests and other-chair run code are excluded.
Rust test volume and the peer failure matrix are the main uncertainty. Delete
store `oracle.rs`/`differential.rs`, compatibility-only `pending`/`field` helpers,
private interpreter facades and parity `PORT_STATUS.md` prose rather than
adding the new design beside them. Baseline Rust file dispositions follow: PR-only `tests/lock-worker.mjs` from #1332
is rejected, not counted as a deletion from this baseline.

Paths below are relative to `tools/front-door/crates/`; all counts are at the
baseline. Rewritten files may be regrouped into the proposed modules.

| Rust file | Lines | Disposition |
|---|---:|---|
| `fabric-mcp/src/lib.rs` | 1 | Replace placeholder |
| `fabric-runtime/src/batch_execution.rs` | 92 | Delete; shared owner or compatibility machinery |
| `fabric-runtime/src/execution.rs` | 156 | Rewrite, keep invariants |
| `fabric-runtime/src/fabric_policy.rs` | 307 | Delete; shared owner or compatibility machinery |
| `fabric-runtime/src/field.rs` | 31 | Delete; shared owner or compatibility machinery |
| `fabric-runtime/src/gates.rs` | 130 | Delete; shared owner or compatibility machinery |
| `fabric-runtime/src/hosts.rs` | 236 | Rewrite, keep invariants |
| `fabric-runtime/src/identity.rs` | 330 | Rewrite, keep invariants |
| `fabric-runtime/src/identity/tests.rs` | 431 | Rewrite, keep invariants |
| `fabric-runtime/src/json_subprocess.rs` | 276 | Delete; shared owner or compatibility machinery |
| `fabric-runtime/src/lane_watch.rs` | 136 | Rewrite, keep invariants |
| `fabric-runtime/src/lib.rs` | 17 | Rewrite, keep invariants |
| `fabric-runtime/src/pending.rs` | 201 | Delete; shared owner or compatibility machinery |
| `fabric-runtime/src/run_registry.rs` | 150 | Replace with query facade; registry moves to run |
| `fabric-runtime/src/run_root.rs` | 315 | Delete; shared owner or compatibility machinery |
| `fabric-runtime/src/success_status.rs` | 93 | Delete; shared owner or compatibility machinery |
| `fabric-store/src/diagnostics.rs` | 223 | Rewrite, keep invariants |
| `fabric-store/src/differential.rs` | 723 | Delete; shared owner or compatibility machinery |
| `fabric-store/src/landing.rs` | 247 | Rewrite, keep invariants |
| `fabric-store/src/lib.rs` | 190 | Rewrite, keep invariants |
| `fabric-store/src/messages.rs` | 384 | Rewrite, keep invariants |
| `fabric-store/src/oracle.rs` | 106 | Delete; shared owner or compatibility machinery |
| `fabric-store/src/pending.rs` | 94 | Delete; shared owner or compatibility machinery |
| `fabric-store/src/sessions.rs` | 171 | Delete; named sessions cut |
| `fabric-store/src/tasks.rs` | 227 | Rewrite, keep invariants |
| `fabric-store/src/tests.rs` | 419 | Rewrite, keep invariants |

## Implementation lanes

F0 freezes the installed product root returned by `~/.local/bin/provenant root`
(currently `~/Repos/provenant`, the laptop primary checkout), including
its installed binary, at the working pre-port snapshot until cut-over. Never pull
that checkout or reinstall over its binary during the port. The front door
executes owners from that root (`tools/front-door/src/main.rs:200–209`), so it
must remain usable for dispatch, long waits and landing. All port work uses
worktrees from `origin/main`; lanes must not update the installed root. Candidate
binaries and state are isolated; integration cut-over requires its own authority.

F0 registry decision **confirmed** by the other chair in
[#1356](https://github.com/mblauberg/provenant/pull/1356) and PORTING R2:
`run` owns registry, admission, attempts and status, with admission and operation
insertion atomic in run's store. Fabric-runtime owns identity and federation
routing of run queries. Run must never depend on a Fabric crate.

Replacement lanes take 1–2 lane-days each, including targeted tests and review.
Delete an old owner when its replacement and retained callers land; install the
integrated system once. One writer per worktree and crate; module partitions
need explicit owners. Manifests, lockfile and shared freezes have one integration
writer. Estimates include reuse.

| Lane | Days | Depends on | Deliverable and deletions |
|---|---:|---|---|
| F0 shared freeze | 1 | Chairs | Core/records signatures, state/config roots, command subtree, dual-era profiles, confirmed registry ownership and installed-root freeze; no runtime deletion |
| F1 mail/task/event store | 2 | F0 | New SQL and atomic APIs; delete `store.ts`, old schema, sessions/graph persistence and their tests, mail/task workers and relevant TS tests; bounded one-shot importer |
| F2 claims/landing store | 2 | F1 | Fences, overlap checks, push operation phases; delete `landing-cli.ts`, lease tests; safe supervisor lock API required |
| F3 identity and run facade | 2 | F0, other-chair run API | Identity and single run query/submit/control; delete identity/run/seat readers; execution/input/batch/resume/owner bridge and pools/catalogue deletion is conditional on the shared run implementation landing with every caller moved |
| F6a transport/operations | 2 | F0, F3 | Fixed SSH protocol, idempotent run operations and host diagnostics; replace transport cases before deleting host helpers |
| F6b home routing | 2 | F1, F2, F6a | Route each coordination operation and pull remote run events at home; then delete hosts/peer TS, `fabric_hosts.py`, `fabric_lanes.py`, wrapper and remaining host tests |
| F7 Git/landing transport | 2 | F2, F6b, supervisor safe API | Verified bundles/fetch and home-host fenced push; delete `fabric_git.py`; replace its cases from host tests |
| F8 MCP | 2 | F0, F1, F3; then F2/F6b/F7 integration | Tools, schemas, stdio transport and new protocol fixtures; delete server/surface/schema exporters, session/graph tools and `__graph-owner`, graph workers/gates, old MCP judge, tool tests and smoke |
| F9 CLI/config cleanup | 2 | F0, F3, config API; then all Fabric APIs | Fabric subtree/top-level run bindings, registration and legacy cut-over, caller/doc rewrites; delete `cli.ts`, bin wrappers, configure script/tests; shared loader/package deletion waits for its owners |
| F10 integrated acceptance | 2 | F1–F9 | Native multi-process fault tests, installed candidate CI, no-interpreter proof; delete remaining Fabric fixtures/tests/manifests and oracle machinery |

Total: 19 lane-days, planning range 19–27 for interface/fault-recovery repair.
Cutting F4/F5 saves 3 days; splitting home routing adds 2. After F0, F1 and F3
can run in parallel; F2 follows F1. Partition runtime modules for F6a/F6b/F7.
F8 transport/schema and F9 args/registration can start against frozen typed fakes;
completion waits for their APIs. CI, review and shared API availability determine
elapsed time. Installation and merging follow the repository workflow.

Caller closure is part of F9/F10: edit skills and hook helpers to the new APIs,
replace front-door help/routes, installer registrations, CI npm/gate invocations,
MCP runbook and Fabric/hosts docs. Live-model callers use the routing-owned
`provenant route list --adapter <id>` command and freeze that command in F0. Keep hook stdin/stdout protocols intact.
`tests/test_runtime_architecture.py`, `tests/test_stale.py`, schema/layering tests,
`tests/conftest.py` and mixed dispatch/provider/host identity tests lose only
Fabric assertions whose callers disappear; their other owners retain them.
Remove obsolete Fabric golden CLI cases and its tool-parameter fixture; shared
`tests/fixtures/fabric-v1/` records retire with their run/records/quota owners.
Add new Rust fixtures. Neither old acceptance receipts nor run directories
are source cleanup targets.

## Open decisions

| Decision | Recommendation | Owner and consequence |
|---|---|---|
| MCP implementation | Keep the small stdio server and no async; no rmcp dependency amendment | Chairs; user approval needed only if proposing to reverse the dependency/no-async decision |
| MCP protocol profiles | Build dual-era 2025-11-25 and 2026-07-28 now; older profile only on demonstrated need | MCP/config owners; verify installed clients before cut-over |
| Shared policy/Node cleanup | Move project policy to config/confinement; leave root npm/Ajv deletion to its actual caller owner | Both chairs; never delete a shared manifest while a retained caller needs it |

No change to the binding table is proposed. Merging this document changes no
executable, live state or installation.

## Cross-crate asks by owner

This is the single dependency request list for the other chair; signatures are
requested contracts, not claims that the current crates implement them.

| Owner | Exact requested API | Types and promise |
|---|---|---|
| `core` | `ProjectId::from_common_dir(HostId, &Path) -> Result<ProjectId, IdentityError>`; `ProjectId::from_directory(HostId, &Path) -> Result<ProjectId, IdentityError>`; `TimestampMs::checked_add(Duration) -> Result<TimestampMs, TimeError>` | Host-local ProjectId, recorded on each run; runtime owns cross-host ProjectKey. Opaque IDs including `RequestId`, `HostId`, `RunId`, `AttemptRef`, `OperationId`, `Digest`, `GitOid`; `WaitBudget`, cooperative `CancelToken`; no I/O or unsafe |
| `records` | `encode_canonical<T: Serialize>(&T) -> Result<Vec<u8>, CodecError>`; `decode_bounded<T: DeserializeOwned>(&[u8], JsonLimits) -> Result<T, CodecError>` | `Serialize`-only edge facts; versioned stored records; duplicate-key/depth/number refusal; `Authority`, `Submission`, `TaskRequest` (`id: RequestId`), `SubmissionReceipt`, `ResumeRequest`, `RunQuery`, `WaitCondition`, `RunSnapshot`, `RunEvent`, `RouteReceipt`, `ArtifactRef`, `ArtifactManifest`, `Observed`, `OutputChunk`, `OperationKey`, `ExecutionContext`, `AttemptTempPaths`, `RegistrationDescriptor` |
| `config` | `load(&ConfigLocation, &EnvSnapshot) -> Result<ResolvedConfig, ConfigError>`; `project_policy(&ResolvedConfig, &Path) -> Result<ProjectPolicy, ConfigError>` | `~/.local/share/provenant/v1`, host-global; `--state-dir` over `PROVENANT_STATE_DIR`; one config.toml, no layering/per-knob env; only product env inputs are PROVENANT_STATE_DIR and PROVENANT_AGENT_LABEL; immutable Fabric config/peers/policy |
| `config` | `reconcile_mcp(&RegistrationDescriptor, &[Client], ApplyMode) -> Result<RegistrationReport, ConfigError>` | `install [--dry-run]`, `check install`, `uninstall`, `--legacy-checkout`; MCP descriptor stays Fabric-owned (PORTING R2); replace/remove legacy hook/skill/workflow/instruction links, rewrite MCP and global instructions/linked files for label rename automatically; preserve unrelated config, print edits, re-install recovers |
| `supervisor` | `entropy_128() -> Result<[u8;16], OsError>`; `lock(&Path, LockMode) -> Result<OwnedLock, OsError>` | OS-owned handle, release only on close/death; no TTL substitute |
| `supervisor` | `private_dir(&Path) -> Result<DirHandle, FsError>`; `publish(&DirHandle, &OsStr, &[u8], PublishMode) -> Result<(), FsError>` | No-follow, permissions, atomic/no-replace modes and explicit durability |
| `supervisor` | `capture(CommandSpec, CaptureLimits, &CancelToken) -> Result<Captured, ProcessError>`; `spawn_owner(OwnerSpec) -> Result<OwnerHandle, ProcessError>` | Bounded pipes/deadline; exact OS identities; detached custody; `stop_owned(&OwnerHandle, CancelReason)` only; `write_frame(&StreamHandle, &[u8], WaitBudget) -> Result<(), IoError>` bounds stdout backpressure |
| `confinement` | `read_input(&Path, &Authority, InputLimit) -> Result<InputBytes, ConfinementError>`; `prepare(&TaskRequest, &ProjectPolicy, &AttemptTempPaths) -> Result<PreparedAccess, ConfinementError>` | Checked roots, symlink refusal, write ownership, secret and instruction policy; called by run |
| `run` | `submit(Submission, OperationKey, &ExecutionContext) -> Result<SubmissionReceipt, RunError>`; `operation(ProjectId, OperationId) -> Result<Observed<SubmissionReceipt>, RunError>`; `recover(ProjectId, OperationId) -> Result<Observed<SubmissionReceipt>, RunError>`; `reconcile(ProjectId, OperationId, ReconcileDecision) -> Result<RunSnapshot, RunError>` | Confirmed registry/admission/attempt/status owner (PORTING R2, #1356); admission and operation insertion atomic in run store; caps owned by r7-execution-run; durable acceptance, one owner, exact receipts |
| `run` | `query(RunQuery) -> Result<Page<RunSnapshot>, RunError>`; `snapshot(ProjectId, AttemptRef) -> Result<RunSnapshot, RunError>`; `wait(RunQuery, WaitBudget, &CancelToken) -> Result<Page<RunSnapshot>, RunError>` | Any/All wakes on terminal or any `InputRequired`; explicit CLI budget ≤14400 s, MCP ≤55 s; met condition exits 0; Fabric-runtime routes remote queries using ProjectKey, run scopes local ProjectId |
| `run` | `cancel(AttemptRef, OperationKey, CancelReason, &ExecutionContext) -> Result<CancelReceipt, RunError>`; `resume(ResumeRequest, OperationKey, &ExecutionContext) -> Result<SubmissionReceipt, RunError>` | Check project/actor/authority, exact target/generation and preserved route/access; no implicit fresh continuation |
| `run` | `read_output(AttemptRef, ArtifactPart, ByteRange, &ExecutionContext) -> Result<OutputChunk, RunError>`; `export(AttemptRef, ExportLimits, &ExecutionContext) -> Result<ArtifactManifest, RunError>` | Check project/actor/authority; bounded regular files, stable manifest digest and immutable published output |
| `run` | `events(RunCursor, usize) -> Result<Page<RunEvent>, RunError>`; `capabilities() -> Result<ExecutionCapabilities, RunError>`; `temporary_paths(ProjectId, AttemptRef) -> Result<AttemptTempPaths, RunError>` | Replayable completion/input-required events, lazy home-host pulls with per-host cursor; aggregate readiness and host-local confinement paths |
| `providers` | `continuation(&AdapterId) -> ContinuationSupport`; `doctor(&AdapterId, ProbeBudget) -> Result<AdapterHealth, ProviderError>` | Used by run; typed session token, supported mode and host-local health, no Fabric dependency |
| `routing` | `resolve(&RouteRequest, &RoutingContext) -> Result<ResolvedRoute, RoutingError>` | Used by run; exact applied route, fallback/confidentiality decisions, one pool implementation |
| `quota` | `admit(&ResolvedRoute, &OperationId) -> Result<AdmissionPermit, QuotaError>` | Used by run; one admission owner across all entry points; permit tied to actual attempt |
| `cli` | `FabricArgs::execute(&FabricService) -> Result<CliOutput, CliError>`; `hook_specs() -> &[HookSpec]` | CLI owns check subjects/Findings and shared installer HookSpec table; top-level dispatch/lanes/output/cancel/resume/reconcile owned by run; CLI envelope schema_version/command/status/data/error, 1 MiB cap; exits 0, 1, 2, 3, 124, 130; partial/input-required in data; no library imports cli |
