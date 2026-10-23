# Fabric Rust design

Status: proposed, 23 October 2026. Audience: the two Rust chairs and Fabric
implementers. This document defines the replacement, its dependency requests and
its deletions. It approves no install, import, live-provider run or landing.

## Direction and boundary

Build Fabric as three libraries behind the single `provenant` executable:
`fabric-store` owns transactions, `fabric-runtime` owns coordination and the
execution facade, and `fabric-mcp` owns stdio MCP. Both CLI and MCP call the same
typed service. Keep durable messages, tasks, fenced ownership, detached runs,
named sessions, bounded DAGs and explicit SSH peers. Remove interpreter bridges,
parallel status engines, permissive input repair and duplicated routing policy.

The v7 chair agreement supersedes the transition and parity portions of
[ADR 0028](../../../adr/0028-rust-port-dependencies.md),
[PORTING v5](../PORTING.md) and the [old plan](../README.md). Their dependency
allowlist, crate layering, typed errors, serde records, bounded inputs and
`supervisor::sys` unsafe boundary still apply. There is no async runtime.
Only provider wire forms, harness hook protocols, Agent Skills format, MCP and
Git survive as external contracts. Our verbs, tools, records and layouts change
together. Main may be red for removed Python/TS paths. Delete each old owner and
its tests when its Rust replacement lands; install once after integration.
Do not build selectors, adapters, recorded old-runtime judges or differential
oracles. For Fabric this replaces PORTING v5 R9/R10 J1/J2/J3 old-pass, mutant
and differential gates with the native verification matrix below. Retained
external MCP/Git/provider/hook contracts still require their own native fixtures. Leave old state, telemetry and transcripts untouched.

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
| Hooks and workflow helpers | Stable hook stdin/stdout, bounded context and notifications | `scripts/configure-claude-hooks.py:100–105` matches Agent/Fabric dispatch; `scripts/routing_context_hook.py:50–61` emits advisory context, without store calls |
| Another host | Explicit placement, host-owned status/cancel, committed code transport and bounded output | `scripts/fabric_hosts.py:250,700`; `scripts/fabric_lanes.py:18`; `scripts/fabric_git.py:24` |
| Installer and six MCP clients | Register the same installed binary while preserving unrelated client configuration | `scripts/configure-fabric-mcp.py:168`; [registration runbook](../../../runbooks/fabric-mcp-registration.md) |
| Other library owners | One run/status API and one policy loader | `fabric_policy.py` is also imported by lint, secret scan and provider execution; its filename does not make those policies Fabric-owned |

The source paths abbreviated as `src/` in this table are below `runtime/fabric/`.
The static call-site scan covers skills, hooks, scripts and docs. Observed usage
and the resulting retention decisions are recorded below; static references do
not establish that a feature is used. Local inspection reads metadata only,
never credentials, prompt bodies or transcript content.

Observed on 23 October 2026 by read-only SQLite aggregate queries against
`~/.local/state/agent-harness/fabric/fabric.sqlite3` (about 21.8 MB):

| Observation | Count | Design implication |
|---|---:|---|
| Messages / delivery rows / delivery claims | 12,456 / 18,181 / 530 | Keep durable mail; a row-only import loses processing state |
| Message kinds | 8,036 terminal, 2,777 response, 430 request | Put terminal notices in events; retain correlated mail |
| Tasks | 475, including 25 open | Keep tasks; fresh state means intentional resubmission of open work |
| Activity / work claims | 17,987 / 454 | Keep one journal and advisory ownership |
| Landing activity | 1,386 acquires and 1,386 releases | Three current lease rows conceal substantial use |
| Run observations / qualified remote observations | 8,242 / 3,327, with 2,003 distinct remote IDs | Keep federation and owning-host status |
| Sessions / session events | 2 / 13 | Keep a small continuation alias |
| Graphs / attempts / reservations | 3 / 11 / 0 | Retain a much smaller DAG; remove revision/gate/reservation machinery |

Counts establish recorded activity, not currently running work. Message/activity
history spans August to 23 October 2026. The OTel directory has 22,329 log records
and 419 metric records across four files totalling about 454 MB; this is raw
telemetry, not proof of a trivially importable ledger. The telemetry chair must
inspect its ledger separately. `~/.cache` contains runtime/build caches, with no
Fabric state discovered. State-side seat/lane caches were counted, never imported.
No credential, message body, prompt or transcript content was inspected.

Reproduce counts with read-only SQLite (`mode=ro`), `count(*)` over messages,
deliveries, delivery_claims, tasks, work_claims, sessions, graphs and graph_attempts;
group activity by kind and count qualified IDs in run_observations. These are a
bounded workstation observation, not a production metrics contract.

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
| cwd identity, common Git project across worktrees | Keep | Derive one project key from canonical Git common directory; worktree remains execution identity | New IDs |
| Seats, provider labels and agent inbox labels | Simplify | Explicit client label plus optional agent label; neither proves model family | Environment changes |
| Mail, reply correlation, task links | Keep | Immutable message plus per-recipient delivery; bounded body and explicit references | Typed envelope |
| Peek, claim, expiry, acknowledge | Keep | Receipt includes an opaque claim token; ack requires the current token | New inbox/ack shape |
| Terminal-run mail notices | Merge | Run completion emits an idempotent activity event; mail only when requested | Fewer unsolicited messages |
| Teams and membership tables | Cut | Explicit recipient lists replace a second address registry | Team addressing removed |
| Tasks, ownership, done and dependencies | Simplify | Four states and token-fenced ownership; graph owns execution dependencies | Task dependency API removed |
| Notes, activity, events and watch | Merge | One append-only project event stream with bounded cursor reads | Four views become one |
| Advisory issue/path work claims | Keep | Generation-fenced claims in the project home store; canonical path scopes | New claim operations |
| Landing lease, generation and expected SHA | Keep | Store fencing plus a home-host push operation and Git lease | New landing subtree |
| Run registry, seat index, recursive lane scans | Merge | Run owner supplies one indexed registry and typed snapshots | Old paths/selectors removed |
| Status, lanes, runs, dispatch list and wait | Merge | One run query plus bounded wait; missing proof stays unknown | Aliases removed |
| Output tails, events and receipts | Simplify | Named artefact enum, byte cursor and cap; no arbitrary path read | New output shape |
| Local/remote fetch | Keep | Explicit verified copy of an allowlisted run artefact set | No whole-directory copy |
| Single dispatch and batch | Merge | One submission containing 1–64 tasks; one run owner handles either | Scalar dispatch form removed |
| Routing pools, councils, rotation and fallbacks in TS | Merge | `routing` resolves typed selections once; Fabric forwards the result | No Fabric-specific precedence |
| Provider catalogue and cooldown reset | Cut from Fabric | `routing` and `quota` CLI own these; no adapter administration MCP tool | `fabric_adapters` removed |
| Dispatch route and authority validation | Merge | `run`, `routing` and `confinement` own it; Fabric validates envelope and project only | Strict typed inputs |
| Native-seat provider bans | Cut | No family inferred from a seat; shared run policy controls permitted dispatch | Same-provider dispatch possible |
| Named sessions and native continuation | Keep | One active turn, route/authority pinned, explicit fresh turn | Separate session operation |
| Ad-hoc handoff prompt assembly | Cut | Chair submits an explicit new task with artefact inputs | `handoff` removed |
| Bounded graph execution | Simplify | Immutable DAG, result edges, width cap, explicit retry and cancellation | No revisions/force resume |
| Graph shell gates, gate workers, auto task mirroring | Cut | Verification is an ordinary authorised task or chair action | Gate document fields removed |
| Graph account/pool admission | Merge | `run` owns admission across graph and ordinary tasks | One capacity ledger |
| SSH hosts and diagnostics | Keep | Explicit trusted host list and one fixed peer command | Versioned new peer envelope |
| Host auto-placement and local fallback | Cut | Default is local; remote placement always explicit | Placement defaults removed |
| Remote launch/cancel/resume idempotency | Keep | Durable operation ID and request digest, unknown launch never retries locally | New operation receipt |
| Remote PID probing | Cut | Only the owning host can report process identity/liveness | Remote stale is explicit |
| Messages/tasks/activity federation | Simplify | Route coordination to one project home, never merge replicated stores | Offline remote coordination refused |
| Offline outbound mailbox, inbox pull and merged host cursors | Cut | No outbox replicator or clock-skew merge; one authoritative sequence | Offline delivery removed |
| Git bundles and bound writer results | Keep | Committed base, isolated refs, verified head and worktree ownership | Explicit transport request |
| Registration, doctor and install smoke | Merge | Config owns edits; Fabric owns descriptor and runtime health | Explicit `fabric mcp` args |
| DB diagnostics and pruning | Simplify | Read-only health; explicit maintenance of new state only | No startup retention sweep |
| Numeric coercion, synonym repair, fuzzy command hints | Cut | Refuse invalid fields with a stable error code and field path | Inputs become strict |
| Python/Node discovery, subprocess RPC and package provisioning | Cut | In-process libraries and one executable | No runtime interpreters |
| Old MCP judges and Rust differential tests | Cut | Rust-native contract fixtures test the new design | Old transcripts retired |

## Libraries and core records

Internal dependencies follow PORTING's table. Do not add `routing`, `quota`,
`telemetry` or `providers` directly to `fabric-runtime`: `run` supplies their
aggregate dispatch and capability APIs. Do not import `cli` into a library.
The executable composes CLI and MCP; `fabric-mcp` is a library, never another
installed binary. Help and root resolution do not open SQLite or start threads.

| Crate | Proposed modules | Responsibility |
|---|---|---|
| `fabric-store` | `db`, `mail`, `tasks`, `claims`, `sessions`, `graphs`, `events`, `operations` | SQL schema, atomic transitions, typed rows; no Git, provider or SSH processes |
| `fabric-runtime` | `identity`, `service`, `runs`, `sessions`, `graph`, `peer`, `git_transport`, `health` | Resolve project, enforce scope, compose store/run/supervisor, bound federation |
| `fabric-mcp` | `protocol`, `stdio`, `tools`, `render`, `registration` | MCP wire lifecycle, schemas, cancellation and compact rendering |
| `cli` Fabric module | `args`, `render` | Clap subtree and text/JSON presentation, calling the same service |

All records crossing a crate or persistence edge derive serde. Shared identifiers
and process/authority types belong in `core` or `records`; private SQL rows stay
in store. New owned formats carry `version: 1`; refuse unsupported major
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
    body: String, reply_to: Option<Qualified<MessageId>>, task: Option<Qualified<TaskId>> }
enum TaskState { Open, Active, Done, Cancelled }
enum NodeState { Pending, Running, InputRequired, Passed, Failed, Blocked, Cancelled }
enum Observed<T> { Current(T), Stale { value: T, observed_at: TimestampMs }, Unknown }
struct Page<T> { items: Vec<T>, next: Option<Cursor> }
struct OutputChunk { bytes: Vec<u8>, next_offset: u64, eof: bool, digest: Digest }
```

Message/task/run/operation IDs are opaque 128-bit values, hex encoded, generated
by the safe supervisor entropy API. HostId is generated once in new config;
ProjectId is the domain-separated SHA-256 repository key. AgentId is a bounded
inbox label, defaulting to the client label; an explicit agent label separates
concurrent inboxes. Shared labels intentionally compete for delivery claims.
Host IDs and display labels are separate. A route receipt comes
from `run`, copied exactly as `adapter/model@effort`; absence is explicit.
Client names, seats and model aliases never become invented route provenance.
Tokens are redacted from diagnostics; only the successful claimant receives one.

### Identity and configuration

Proposal for shared freeze: new state root `${XDG_STATE_HOME:-~/.local/state}/provenant/v1`;
Fabric owns `fabric.sqlite3`, `fetch/` and `locks/fabric/` inside it. `run` owns
`runs/`, telemetry owns its ledger. Root selection comes only from config.
A candidate always takes an explicit isolated root. No implicit discovery of
`~/.agents`, `.agent-run` or old Fabric databases.

Project identity hashes the canonical Git common directory relative to home,
with a domain tag; linked worktrees share the key. Outside home use the canonical
absolute path for local identity and refuse remote placement. Non-Git directories
use canonical cwd, local only. Federation v1 requires matching home-relative
repository paths and verifies the project key at the peer. Moving a repository
creates a new project unless the user explicitly rebinds it through config.
No registry scan or remote URL heuristic silently joins projects.

Config owns `FabricConfig { agent_label, client_label, home_hosts, peers, limits }`.
Precedence: explicit command options, `PROVENANT_STATE_ROOT` and
`PROVENANT_AGENT_LABEL`, user config, product defaults. Exact shared names need
the chairs' freeze. Remove `AGENT_FABRIC_*`, `CF_DISPATCH`, Node/Python knobs and
legacy owner selectors from the new binary. The installer writes explicit
`fabric mcp --client <label>` arguments, never a no-argument environment trigger.
Project policy for instruction changes, secret-scan exclusions and lint moves
from `fabric_policy.py` to `config::ProjectPolicy`; its enforcement remains in
confinement/run and cli respectively. Invalid safety policy refuses launch.

Defaults: body 64 KiB; input frame 1 MiB; JSON nesting 64; page 100 rows; output
64 KiB per read, maximum 256 KiB; 64 tasks/nodes; graph width 4, hard maximum 16; submission concurrency 1–16;
request wait 0–30 seconds; lease 1–3,600 seconds; peer RPC deadline 30 seconds;
Git bundle limit 64 MiB. Config may lower hard limits, never raise them.
Prompts and artefact inputs also obey the run owner's aggregate request cap.
Run admission enforces cross-process hard caps of 64 active attempts/host,
16 per project on each host and 256 queued tasks/host, including every entry point.
Starting or uncertain live attempts retain capacity until reconciled. Admission
and operation insertion are atomic in run's own durable registry; Fabric has no
second capacity ledger. Reject excess queue work before acknowledging acceptance.

## Store and state layout

One host-local SQLite database replaces the Fabric and host-operation databases.
Use bundled rusqlite, WAL, `foreign_keys=ON`, `synchronous=FULL`, busy timeout
at most 250 ms per transaction. Private root mode 0700, database and WAL/SHM
0600, created through supervisor safe filesystem APIs. Refuse symlinked state
components. No network filesystem SQLite and no shared connection across threads.
`Store` is a concrete connection owner, opened per worker; no generic backend
trait for a single database. Use numbered embedded migrations and `user_version`;
reject a future version before any mutation. First open serialises schema setup.

| Table | Key and important fields | Constraint or index |
|---|---|---|
| `projects` | `id`, common-dir key, home host | Unique canonical key; home changes require no active ownership |
| `agents` | `(project, host, agent)`, label, client, last_seen | Presence is observation, never process liveness |
| `messages` | `id`, project, sender, body, reply/task refs, created | Immutable; references may be qualified external values |
| `deliveries` | `(message, recipient)`, ack time, claim token/generation/expiry | Unique delivery; index recipient/unacked; claim and ack compare generation/token |
| `tasks` | `id`, project, objective, state, owner, generation/token/expiry | State CHECK; CAS ownership; no task dependency table |
| `claims` | `id`, project, kind, scope, holder, generation/token/expiry, phase | Work scope rows canonicalised; one landing row per project/ref |
| `claim_paths` | `(claim, path)` | Prefix overlap checked under immediate transaction; `/a` does not overlap `/ab` |
| `sessions` | `(project, name)`, host, pinned route/access, continuation, last_clean, active operation, generation | One active turn; completion compares operation and generation |
| `graphs` | `id`, project, canonical document/digest, state, width | Immutable document; single scheduler owner lock |
| `nodes` | `(graph, node)`, request, state, run, attempt, input bindings | Dependencies in `edges(graph, from, to)`; unique edge; DAG validated before insert |
| `operations` | `(project, id)`, kind, bounded frozen request, digest, phase, result | Same ID/different digest refused; prepared/sent/committed/unknown |
| `events` | autoincrement `seq`, project, kind, entity, payload, time, dedup key | Unique dedup key; `(project, seq)` cursor index |
| `peer_cache` | `(project, host, query)`, observation, bounded snapshot | Cache is never authority; replaced only by valid responses |

All related mutation and event insertion commit in one transaction. A failed
event write rolls back the coordination mutation. No transaction stays open
while waiting, spawning a process, probing Git or making SSH calls. Generation
increments survive release and expiry; never delete/recreate the counter row.
Claims use the home host's clock only. Runtime waits use monotonic deadlines.
Run snapshots and terminal records remain `run`'s authority; Fabric stores IDs
and indexes, not another mutable run status. Missed completion events can be
replayed from run's durable cursor using a unique dedup key.

A message claim is at-least-once processing. Peek has no mutation. Claim replaces
only an absent or expired token atomically; ack compares recipient, token and
generation. An expired claimant cannot acknowledge a redelivery. Task `finish`
requires the active task fence; changing owner requires release or expiry.
Work claims are advisory coordination, never authority to write a worktree;
`run` still obtains the actual supervisor writer lock.

### Fresh state and optional import

Start empty. Leave old `~/.agents`, `.agent-run`, telemetry databases and
transcripts untouched, including during rollback and maintenance. Do not import
runs, sessions, process handles, claims, leases, graphs, config or cooldowns.
Recreate peers and labels through new config; resubmit intentional active work.

Messages and tasks have user value, but a complete operational import is **not
trivial**: project IDs and agent addresses change, deliveries have claims and
external references, and old tasks have dependencies/ownership semantics we cut.
Recommendation: omit the importer. If requested, a one-shot Rust command could
read an explicit SQLite snapshot read-only and copy messages plus open task text
as historical records with old IDs in metadata, no live claims or owners. Cost:
0.5–1 lane-day for mapping, idempotence report and validation, plus decisions on
address mapping. It must neither mutate the old store nor become a fallback reader.
Telemetry ledger import belongs to the telemetry chair, is permitted only if
trivial, and must not pull transcripts or reconstruct route identities. Fabric
has no reason to read it at runtime. Fresh state is the acceptance default even
if either optional import is rejected.

## CLI and MCP surfaces

This is a **proposed subtree**, subject to the other chair's command-tree freeze.
Request commands support `--json`; MCP and peer modes use protocol framing.
Mutations accept `--operation <id>` where retries matter. Text is compact; JSON uses versioned typed records and stable error codes.
No old aliases remain. CLI prompt files are read by confinement and become typed
prompt bytes before service invocation; MCP accepts text and typed artefact refs.

| Proposed `provenant fabric …` | Operations |
|---|---|
| `identity` | Project, worktree, host and inbox address |
| `mail send\|read\|claim\|ack` | Explicit recipient(s), peek/list, fenced delivery processing |
| `task create\|list\|claim\|update\|finish\|release` | Objective and lifecycle; update cannot bypass ownership |
| `events read\|note` | Cursor, limit, optional wait and append-only notes |
| `claim acquire\|renew\|verify\|release\|list` | Advisory issue/path work ownership |
| `landing acquire\|renew\|verify\|release\|push\|reconcile` | Branch/expected SHA/fence; push stays an explicit authorised CLI action |
| `run submit\|list\|status\|wait\|cancel\|output\|fetch\|resume\|reconcile` | Delegates to the shared run owner; selectors are exact qualified IDs |
| `session list\|show\|turn\|forget` | Turn mode `continue` or `fresh`; forget refuses an active turn |
| `graph submit\|show\|retry\|cancel` | Immutable DAG; retry names failed nodes and retains successful inputs |
| `host list\|doctor` | Read-only peer and adapter readiness |
| `doctor` | Store health, owner/peer uncertainty, configuration faults |
| `mcp` | stdio server mode of this executable |
| hidden `peer`, `__graph-owner` | Fixed framed peer ingress and detached scheduler entry; no user command passthrough |

`run submit` uses one `Submission { tasks: Vec<TaskRequest>, concurrency }`;
no scalar/list union. A single task is a one-item list. Resume is a separate
`ResumeRequest { target: AttemptRef, prompt, expected_generation }`; it cannot
change route, account or authority. Fresh session turns create new runs with an
explicit submitted request. Graph retry creates a new attempt, not a mutable
plan revision or a new authority grant.

Expose 12 tools instead of today's 20. Each tool's tagged input enum rejects
unknown fields; generate its checked JSON schema from an explicit shared field
catalogue without another schema crate. Snapshot/schema tests bind the catalogue
to serde acceptance. No `detail`, scalar/list aliases or numeric-string coercion.

| Tool | Typed operations | Replaces |
|---|---|---|
| `fabric_identity` | inspect | whoami |
| `fabric_mail` | send, read, claim, ack | send, inbox, acknowledge |
| `fabric_task` | create, list, claim, update, finish, release | task |
| `fabric_events` | read, note | events, note, activity |
| `fabric_claim` | acquire, renew, verify, release, list | work_claim |
| `fabric_landing` | acquire, renew, verify, release | landing_lease; no push tool |
| `fabric_submit` | submission | dispatch, including batch |
| `fabric_run` | list, status, cancel, resume | runs, status, cancel |
| `fabric_output` | read, fetch | output, fetch |
| `fabric_session` | list, show, turn, forget | session plus named dispatch options |
| `fabric_graph` | submit, show, retry, cancel | graph |
| `fabric_hosts` | list, doctor | hosts |

Responses contain typed `structuredContent` and a short text digest: IDs, state,
exact route if known, and next cursor/artefact references. Do not repeat result
tails in every status call. Output bytes are UTF-8 text when valid; otherwise an
explicit byte-array encoding, never lossy text. Fetch copies only run-exported
regular files, refuses symlinks/traversal and enforces total byte/file caps. It
verifies a manifest digest before atomic publication below `fetch/<host>/<run>/`.
No tool accepts an arbitrary source or destination filesystem path.

### MCP protocol and SDK decision

Recommend an in-house, bounded stdio JSON-RPC transport and **no rmcp amendment**.
The official [rmcp manifest](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/Cargo.toml)
currently requires Tokio, futures and tracing even with optional features disabled.
Using it would require both a dependency amendment and reversal of no-async.
Its maintained protocol types and lifecycle machinery are valuable, but the
single stdio tools-only server does not justify that runtime expansion. Revisit
if HTTP transport, elicitation or subscriptions become actual requirements.
Budget 700–1,000 production lines for framing, validation, lifecycle and schemas;
if it grows materially beyond this, return the maintenance trade-off to the chairs.

Pin the supported external MCP versions in one constant and native protocol
fixtures. Initial recommendation: `2025-11-25` and `2025-06-18` lifecycle profiles,
verified against installed clients before cut-over. The
[2025-11-25 transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
requires UTF-8 newline JSON-RPC, protocol-only stdout and diagnostics on stderr.
Handle initialise/initialised, version and capability negotiation, ping,
`tools/list`, `tools/call`, cancellation notifications and EOF. No resources,
prompts, sampling or task extension is advertised. Standard JSON-RPC parse,
invalid request/params and method errors stay distinct from tool execution
failures (`isError: true`). Preserve string/integer request IDs exactly.

The upstream [2026-07-28 stdio specification](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/basic/transports/stdio.mdx)
uses per-request metadata rather than the older initialise lifecycle. Do not
advertise it through the legacy handler. Add its separate small protocol profile
only if client verification requires it; freeze that choice before MCP coding.
Cancellation stops observation or a not-yet-committed operation, suppressing late
responses according to the negotiated protocol; cancelling an MCP request never
implicitly cancels a launched run. An explicit run cancel remains necessary.

## Concurrency, custody and recovery

No Fabric daemon, listener, async executor or in-memory authoritative registry.
One MCP process serves one client. A bounded stdin reader and one stdout writer
surround a pool of four request workers with queue capacity 32; cancellation and
protocol control use a separate bounded control path so long waits cannot block
them. Saturation returns a typed busy error. Workers own their connections.
Framing reads at most the cap plus one byte before refusal; no unbounded
`read_line`. Duplicate live request IDs are refused. The writer serialises whole
frames with a bounded write deadline; a wedged reader ends this transport. On EOF cancel observations, stop admission, join bounded workers and exit;
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
or uncertain identity means starting/unknown, never a second provider launch. For intent-without-start uncertainty, explicit `run reconcile` may abandon the
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

Named sessions reserve `(project, name, generation)` before launch, then bind the
operation/run. Recovery queries the operation after a lost response. Continuation
pins provider session, applied route, authority and last clean attempt; an
unsupported adapter returns `ContinuationUnsupported`, never silently starts
fresh. A failed or cancelled turn does not overwrite the last clean pointer.
Concurrent turns yield one winner. Forgetting a session deletes only its new
pointer after active-turn refusal; run artefacts remain under run retention.

A graph validates the whole immutable DAG before persisting: unique nodes,
known edges, no cycles, maximum 64 nodes/depth 64, authority no broader than the
chair, resolved worktrees, and bounded prompt/result inputs. One detached
scheduler holds an OS lock per graph. It deterministically selects ready nodes
by node ID, submits through `run`, records the operation before launch and
reconciles every unfinished operation on restart. Run owns admission and writer
locks. Failed nodes block descendants; independent branches continue.
`InputRequired` pauses that branch with a visible run reference. Cancellation
marks intent before requesting run cancellation and waits for publication before
releasing logical ownership. Scheduler death cannot release a live writer lock
or infer a node passed. Retry uses the same document and pinned passed inputs;
new routing or authority requires a new graph. No automatic resubmission of an
unknown launch and no shell gate process engine.

### Peers and Git

Each configured peer is the same user's trusted host, accessed through system
SSH, no listener. Supervisor captures bounded stdin/stdout/stderr using a fixed
argv and deadline. The forced remote command is `provenant fabric peer`; it
accepts one versioned request, not a command string. SSH destination comes only
from validated config. Provider credentials stay on the executing host.
Handshake verifies protocol major, host ID and project key. Version mismatch
refuses writes and marks cached reads incompatible. Peer fan-out uses at most
four transport threads, never holds a DB transaction, and never recursively fans
out from a peer request. Control cancellation stops the SSH child, not its remote
run. Remote owner status is authoritative; stale snapshots include age.

All project coordination routes to its configured home, default local. Messages,
tasks, events, claims and landing share that authority; a non-home host does no
local mirror writes. An unreachable home returns `HomeUnavailable`, including
for mailbox send. Local run execution remains available. Recipient HostId identifies an inbox address, not its storage location: send,
read, claim and ack all target the project home, including remote recipients.
No pending message queue, distributed task claim, merged clock ordering or
home-host auto-election.
Home reassignment refuses active ownership and requires an explicit new-state
transfer decision. This is the main federation scope reduction to approve.

Remote mutations use `(OperationId, request digest)`. Persist prepared, mark sent
before transport, and query the owning host on uncertainty. Same ID/different
body is a conflict. No fallback after a request might have crossed the boundary.
The peer persists operation acceptance before spawning a run; the run owner's
own idempotency key closes the database/process gap. Read timeouts can return
stale observations; writes never report success from a cache.

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

| Owner | Exact requested API | Types and promise |
|---|---|---|
| `core` | `ProjectId::from_common_dir(&Path, &Path) -> Result<ProjectId, IdentityError>`; `ProjectId::from_directory(&Path, &Path) -> Result<ProjectId, IdentityError>`; `TimestampMs::checked_add(Duration) -> Result<TimestampMs, TimeError>` | Opaque IDs, `HostId`, `RunId`, `AttemptRef`, `OperationId`, `Digest`, `GitOid`; no I/O or unsafe |
| `records` | `encode_canonical<T: Serialize>(&T) -> Result<Vec<u8>, CodecError>`; `decode_bounded<T: DeserializeOwned>(&[u8], JsonLimits) -> Result<T, CodecError>` | Duplicate-key/depth/number refusal; `Authority`, `RouteReceipt`, `ArtifactRef`, `ArtifactManifest`, `RunSnapshot`, `RunEvent` |
| `config` | `load(&ConfigLocation, &EnvSnapshot) -> Result<ResolvedConfig, ConfigError>`; `project_policy(&ResolvedConfig, &Path) -> Result<ProjectPolicy, ConfigError>` | `StateRoot`, `FabricConfig`, peers, policy and hard limits, immutable resolved values |
| `config` | `reconcile_mcp(&RegistrationDescriptor, &[Client], ApplyMode) -> Result<RegistrationReport, ConfigError>` | Preserve unrelated config/comments; stale-file refusal and explicit partial-state report |
| `supervisor` | `entropy_128() -> Result<[u8;16], OsError>`; `lock(&Path, LockMode) -> Result<OwnedLock, OsError>` | OS-owned handle, release only on close/death; no TTL substitute |
| `supervisor` | `private_dir(&Path) -> Result<DirHandle, FsError>`; `publish(&DirHandle, &OsStr, &[u8], PublishMode) -> Result<(), FsError>` | No-follow, permissions, atomic/no-replace modes and explicit durability |
| `supervisor` | `capture(CommandSpec, CaptureLimits, &CancelToken) -> Result<Captured, ProcessError>`; `spawn_owner(OwnerSpec) -> Result<OwnerHandle, ProcessError>` | Bounded pipes/deadline; exact OS identities; detached custody; `stop_owned(&OwnerHandle, CancelReason)` only |
| `confinement` | `read_input(&Path, &Authority, InputLimit) -> Result<InputBytes, ConfinementError>`; `prepare(&TaskRequest, &ProjectPolicy, &AttemptTempPaths) -> Result<PreparedAccess, ConfinementError>` | Checked roots, symlink refusal, write ownership, secret and instruction policy; called by run |
| `run` | `submit(Submission, OperationKey, &ExecutionContext) -> Result<SubmissionReceipt, RunError>`; `operation(ProjectId, OperationId) -> Result<Observed<SubmissionReceipt>, RunError>`; `recover(ProjectId, OperationId) -> Result<Observed<SubmissionReceipt>, RunError>`; `reconcile(ProjectId, OperationId, ReconcileDecision) -> Result<RunSnapshot, RunError>` | Durable acceptance and exactly one owner per operation; resolved receipts, no `Value` request bags |
| `run` | `query(RunQuery) -> Result<Page<RunSnapshot>, RunError>`; `snapshot(ProjectId, AttemptRef) -> Result<RunSnapshot, RunError>`; `wait(RunQuery, WaitBudget, &CancelToken) -> Result<Page<RunSnapshot>, RunError>` | Registry owns discovery/status; qualified local IDs only; Fabric routes remote calls |
| `run` | `cancel(AttemptRef, OperationKey, CancelReason) -> Result<CancelReceipt, RunError>`; `resume(ResumeRequest, OperationKey) -> Result<SubmissionReceipt, RunError>` | Exact target and generation, preserved route/access, no implicit fresh continuation |
| `run` | `read_output(AttemptRef, ArtifactPart, ByteRange) -> Result<OutputChunk, RunError>`; `export(AttemptRef, ExportLimits) -> Result<ArtifactManifest, RunError>` | Bounded regular files, stable manifest digest and immutable published output |
| `run` | `events(RunCursor, usize) -> Result<Page<RunEvent>, RunError>`; `capabilities() -> Result<ExecutionCapabilities, RunError>` | Replayable completion cursor; aggregate provider/routing readiness for Fabric doctor |
| `providers` | `continuation(&AdapterId) -> ContinuationSupport`; `doctor(&AdapterId, ProbeBudget) -> Result<AdapterHealth, ProviderError>` | Used by run; typed session token, supported mode and host-local health, no Fabric dependency |
| `routing` | `resolve(&RouteRequest, &RoutingContext) -> Result<ResolvedRoute, RoutingError>` | Used by run; exact applied route, fallback/confidentiality decisions, one pool implementation |
| `quota` | `admit(&ResolvedRoute, &OperationId) -> Result<AdmissionPermit, QuotaError>` | Used by run; one admission owner, graph cannot bypass it; permit tied to actual attempt |
| `telemetry` | `record(&RunEvent) -> Result<(), TelemetryError>` | Used by run, idempotent ledger attribution; telemetry failure is diagnostic, never a provider kill |
| `cli` | `FabricArgs::execute(&FabricService) -> Result<CliOutput, CliError>` | Shared command-tree freeze; edge owns exit/text/JSON, no library imports cli |

The principal shared request/result types are concrete serde records in
`records`; numeric wrappers below are checked at construction. Extend these
through the shared freeze, rather than adding per-edge JSON bags:

```rust
struct Submission { project: ProjectId, tasks: Vec<TaskRequest>, concurrency: u16 }
struct TaskRequest { id: TaskId, prompt: InputBytes, inputs: Vec<ArtifactRef>,
    route: RouteRequest, access: Authority, cwd: PathBuf, timeout: DurationMs }
struct SubmissionReceipt { runs: Vec<AttemptRef>, routes: Vec<RouteReceipt> }
struct ResumeRequest { target: AttemptRef, prompt: InputBytes, expected_generation: u64 }
enum ReconcileDecision { AbandonUncertain }
struct RunQuery { project: ProjectId, ids: Vec<AttemptRef>,
    states: Vec<RunState>, cursor: Option<Cursor>, limit: u16 }
struct RunSnapshot { project: ProjectId, target: AttemptRef, state: RunState, owner: OwnerObservation,
    provider: ProviderObservation, publication: PublicationState,
    route: Option<RouteReceipt>, artifacts: Vec<ArtifactRef>, observed_at: TimestampMs }
struct RegistrationDescriptor { executable: PathBuf, args: Vec<String>,
    env: BTreeMap<String, String> }
```

`Page`, `Observed`, `OutputChunk`, `OperationKey` and `ResumeRequest` also live in
records, so run never imports Fabric. `WaitBudget { deadline: MonotonicDeadline }`
and cooperative `CancelToken` live in core; supervisor consumes them. Runtime's
peer client takes `PeerRequest { version, target_host, project, operation, action }`;
`action` is a closed enum for coordination, run, health and Git transfer. There
is no arbitrary executable field. The target is the execution host for runs and the project home for coordination.
Before submit, Fabric matches Submission.project to Identity.project; run checks
the same binding against `ExecutionContext { project, actor, authority }`, reloads
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
the 12 tool domains, and `registration(executable: &Path, client: ClientLabel) ->
RegistrationDescriptor`. `fabric-store::Store::open(StateRoot)` and transaction
methods take explicit `TimestampMs`; clocks are injected at the service edge.
`CancelToken`, `WaitBudget`, `CommandSpec` and filesystem handles are safe
supervisor/core types. The CLI and MCP own no filesystem policy.
If a signature requires an extra allowed dependency edge, settle the shared
layering table before implementation; do not introduce a hidden edge or `pending`.

## Errors and diagnostics

Libraries use thiserror enums with typed sources and relevant IDs/paths, without
raw secret values. Expected contention, stale fences and uncertainty are data.
Common service codes: `invalid_input`, `not_found`, `conflict`, `busy`,
`stale_fence`, `home_unavailable`, `peer_version`, `launch_unknown`,
`continuation_unsupported`, `confinement_refused`, `state_corrupt`,
`publication_failed`, `cancelled`, `io`. Return retry advice only when safe.
Include `operation_id` on uncertain mutations. CLI maps success to 0, invalid
input to 2, contention/conflict to 3, unavailable/unknown to 4, other failures to 1,
and interruption to 130; these values need the shared freeze. MCP preserves
JSON-RPC errors separately from domain tool errors. Diagnostics go to stderr,
with no panics, production unwraps or swallowed failed writes. Health checks do
not repair, prune or acquire coordination claims.

## Reuse and deliberate stripping

| Material | Keep | Strip or rewrite |
|---|---|---|
| Existing `fabric-store` plus #1332 | Immediate transactions, token/fence checks, overlap tests, contention classification, atomic message+event writes, checked filesystem opening and push critical-section idea | Included TS schema, compatibility migrations, JS truthiness/number/date spelling, legacy terminal-row mapper, backend trait for one store, all oracle/differential machinery |
| Existing `fabric-runtime` plus #1334 | Git redirect scrubbing, common-dir identity, bounded inputs, route/access validation invariants, peer version checks, Git object/ref validation, graph cycle/authority checks | Python owner argv builders, `.agent-run`/product-root/seat discovery, status dictionaries, `Value` field helpers, JS semantics in `pending`, subprocess JSON shims, duplicated pool/routing logic |
| #1344 workflow | Bounded hook envelope parsing, typed authority validation, conservative reinjection and disclosure checks where workflow still owns them | Do not move receipt/session/review schemas into Fabric; no copied Python globals or oracle processes; graph events are not acceptance receipts |
| Existing supervisor/run/records | Safe process identity, custody and publication, writer lock, shared codecs and run records after interface review | Fabric-specific `ps` wrappers and speculative alternate run readers once run owns snapshots |
| Empty `fabric-mcp` crate | Workspace slot and layering | Design implementation from the supported MCP profiles; there is no server code to preserve |

Tests in these PRs supply scenarios, not expected old-runtime outputs. Re-express
invariants as new Rust-native tests. Do not cherry-pick their parity documents or
portable compatibility helpers merely to claim lines reused.

## Verification

No new dependency is needed. Use `cargo test`, std test harness, `tempfile`,
`rusqlite` and checked-in `.json`/`.txt` fixtures. Snapshot assertions compare new
CLI/MCP results to hand-reviewed new-design fixtures; a small helper binds only
fixture-injected IDs and clocks. No insta, proptest, loom or cargo-nextest
amendment is proposed. A separate property-testing dependency can be reconsidered
if seeded deterministic graph/interleaving tests prove insufficient.

| Layer | Rust-native tests and fault cases |
|---|---|
| Store unit | Mail redelivery and stale ack, task owner CAS, path overlap boundaries, generation monotonicity, event rollback, schema future-version refusal, UTF-8/number limits |
| Process contention | Spawn the same test executable as child workers, barrier via pipes; race cold opens, claim/ack, session turns and landing holders; exactly one winner, no timing-only sleeps |
| State faults | Inject write/fsync/rename failure and SQLite busy/full errors through narrow safe-I/O seams; kill fixture owners between each recorded transition; reopen and reconcile |
| Run facade | Fake run owner with durable operation IDs; lost launch reply, dead launcher/live provider, corrupt receipt, cancellation during publication; no duplicate launch or healthy-provider signal |
| Sessions and graphs | Failed turn retains last clean pointer, concurrent turns, unsupported continuation, cycles, cap/authority refusal, blocked descendants, input required, scheduler death and explicit retry |
| MCP | Duplex pipes to the built binary; every supported lifecycle, capabilities, tools/list schemas, strict calls, parse/oversize/depth errors, string/integer IDs, cancellation, full queue, EOF and broken stdout |
| CLI snapshots | Help is side-effect free; exact new subtree and errors, JSON schema/version, output caps/cursors and exit mapping; no Python/Node in PATH |
| Federation | Fake SSH executable, protocol mismatch, skewed clocks, unreachable home, delayed/lost response, idempotent launch reconciliation and bounded cache reads; owning-host liveness only |
| Git and fetch | Local temporary repos and forced peer command; dirty writer refusal, wrong OID/ref, bundle cap, symlink traversal, digest mismatch, moved remote, push reply loss and blocked unresolved fence |
| Install | Config-owned six-client fixtures, comments/unrelated entries preserved, explicit binary mode, partial-state recovery, stale install detection over every workspace source/manifests/lockfile |

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

Whole-file total: **132 files, 35,915 lines**. This includes all
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
including tests. The replacement budget is 13,000–17,000 Rust production/test
lines across the three crates, plus 1,000–2,000 lines in the CLI/config edges.
That target includes retained Rust, not only newly authored lines. Removing the
old files and replacing the existing Rust gives approximately 22,601–27,601 lines of
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
| `fabric-runtime/src/run_registry.rs` | 150 | Rewrite, keep invariants |
| `fabric-runtime/src/run_root.rs` | 315 | Delete; shared owner or compatibility machinery |
| `fabric-runtime/src/success_status.rs` | 93 | Delete; shared owner or compatibility machinery |
| `fabric-store/src/diagnostics.rs` | 223 | Rewrite, keep invariants |
| `fabric-store/src/differential.rs` | 723 | Delete; shared owner or compatibility machinery |
| `fabric-store/src/landing.rs` | 247 | Rewrite, keep invariants |
| `fabric-store/src/lib.rs` | 190 | Rewrite, keep invariants |
| `fabric-store/src/messages.rs` | 384 | Rewrite, keep invariants |
| `fabric-store/src/oracle.rs` | 106 | Delete; shared owner or compatibility machinery |
| `fabric-store/src/pending.rs` | 94 | Delete; shared owner or compatibility machinery |
| `fabric-store/src/sessions.rs` | 171 | Rewrite, keep invariants |
| `fabric-store/src/tasks.rs` | 227 | Rewrite, keep invariants |
| `fabric-store/src/tests.rs` | 419 | Rewrite, keep invariants |


## Implementation lanes

These are replacement lanes, each 1–2 lane-days including targeted tests and one
review/repair. They are not caller-switch slices. A lane deletes its old owner
when its replacement lands; the final install waits for the integrated system.
One writer per worktree and crate; disjoint module partitions need explicit owner
assignment. Workspace manifests, lockfile and shared type/command freezes have
one integration writer. Estimates include reuse, not a line-for-line port.

| Lane | Days | Depends on | Deliverable and deletions |
|---|---:|---|---|
| F0 shared freeze | 1 | Chairs | Core/records signatures, state/config roots, command subtree, protocol profiles; no runtime deletion |
| F1 mail/task/event store | 2 | F0 | New SQL and atomic APIs; delete `store.ts`, old schema, mail/task concurrency workers and relevant TS store tests |
| F2 claims/landing store | 2 | F1 | Fences, overlap checks, push operation phases; delete `landing-cli.ts`, lease tests; safe supervisor lock API required |
| F3 identity and run facade | 2 | F0, other-chair run API | Identity and single run query/submit/control; delete identity/run/seat readers, execution/input/batch/resume/owner bridges, pools/catalogue and their tests |
| F4 sessions | 1 | F1, F3 | CAS turn reservation, reconciliation, continuation; delete `sessions.ts` and named-session tests |
| F5 DAG | 2 | F1, F3 | Immutable scheduler, graph persistence/recovery; delete five `graph/*.ts` files, graph workers/tests and shell-gate machinery |
| F6 peer control | 2 | F0, F3 | Fixed SSH protocol, idempotent operations/cache/home routing; delete hosts/peer TS and `fabric_hosts.py`, `fabric_lanes.py`, wrapper and host tests |
| F7 Git/landing transport | 2 | F2, F6, supervisor safe API | Verified bundles/fetch and home-host fenced push; delete `fabric_git.py`; replace its cases from host tests |
| F8 MCP | 2 | F0, F1, F3; then F2/F4/F5/F6/F7 integration | Tools, schemas, stdio transport and new protocol fixtures; delete server/surface/schema exporters, old MCP judge, tool tests and smoke |
| F9 CLI/config cleanup | 2 | F0, F3, config API; then all Fabric APIs | New subtree, explicit registration descriptor, caller/doc rewrites; delete `cli.ts`, bin wrappers, configure script/tests; shared loader/package deletion waits for its owners |
| F10 integrated acceptance | 2 | F1–F9 | Native multi-process fault tests, installed candidate CI, no-interpreter proof; delete remaining Fabric fixtures/tests/manifests and oracle machinery |

Total: 20 lane-days, planning range 20–28 for interface/fault-recovery repair,
plus the optional importer only if requested. F1 and F3 can start in parallel
following F0. F2 serialises behind F1's store writer. F4 and F5 need distinct
runtime module partitions; F6 and F7 can use their own partitions. F8's transport
and schema work can start against frozen typed fakes after F0; only completion
waits for all service operations. F9's argument/registration work can likewise
start early. Heavy CI, review and the other chair's API availability determine
elapsed time, not the number of agents. No lane installs or merges another
chair's crate without the repository workflow gate.

Caller closure is part of F9/F10: edit skills and hook helpers to the new APIs,
replace front-door help/routes, installer registrations, CI npm/gate invocations,
MCP runbook and Fabric/hosts docs. Keep hook stdin/stdout protocols intact.
`tests/test_runtime_architecture.py`, `tests/test_stale.py`, schema/layering tests,
`tests/conftest.py` and mixed dispatch/provider/host identity tests lose only
Fabric assertions whose callers disappear; their other owners retain them.
Remove obsolete Fabric golden CLI cases and its tool-parameter fixture; shared
`tests/fixtures/fabric-v1/` records retire with their run/records/quota owners.
Add new Rust fixtures. Neither old acceptance receipts nor run directories
are source cleanup targets.

## Decisions for the chairs and user

| Decision | Recommendation | Owner and consequence |
|---|---|---|
| Shared command and state/config freeze | Adopt the proposed Fabric subtree and explicit `fabric mcp`; keep common run execution owned by run | Both chairs; settle duplicate top-level run verbs before implementation |
| MCP implementation | Keep the small stdio server and no async; no rmcp dependency amendment | Chairs; user approval needed only if proposing to reverse the dependency/no-async decision |
| MCP protocol profiles | Verify installed clients, freeze legacy versions above; implement modern profile only when needed | MCP/config owners; MCP itself remains an external contract |
| Offline federation | Use online project-home coordination and remove replicated mail/tasks/events/outbox | User/chairs; visible loss of offline cross-host send and merged task views buys substantial deletion |
| Graph scope | Keep immutable bounded DAGs; cut shell gates, revisions and automatic task mirroring | Chairs; migrate verification tasks to ordinary run requests |
| Import | Start empty; omit message/task importer because complete semantics are not trivial | User; telemetry chair separately assesses a trivial ledger-only import |
| Shared policy/Node cleanup | Move project policy to config/confinement; leave root npm/Ajv deletion to its actual caller owner | Both chairs; never delete a shared manifest while a retained caller needs it |
| Dependency and rules amendment | Amend superseded transition/parity prose once, centrally; add no Fabric crates to the allowlist | Shared integration owner; all implementation PRs use the v7 authority recorded here |

These recommendations are reviewable design choices. Merging this document
alone changes no executable, external protocol, live state or installation.
