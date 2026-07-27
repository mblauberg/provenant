# Agent Fabric daemon and wire

The direct implementation instruction of 11 July 2026 authorises this daemon contract. The Agent Fabric specifications own the
protocol entities and atomic coordination invariants. This specification owns their persistence, lock-safe on-demand
daemon bootstrap, global liveness and stop predicates, notification outbox and crash recovery.

## Lock-safe on-demand bootstrap

The first operator or Fabric client read or command shall call one shared bootstrap client. It shall:

1. attempt a bounded protocol initialisation against the trusted Unix socket;
2. if no compatible incumbent answers because the socket is absent, stale, unreachable, timed out or negotiates an
   incompatible feature version, contend for a generation-bearing lease under the private runtime directory and the
   canonical exclusive daemon-election lock;
3. after acquiring the lease, check the socket again before spawning;
4. spawn the configured daemon with a stable bootstrap action ID;
5. wait for a successful version/capability handshake and authoritative daemon instance generation; and
6. release the bootstrap lease only after success or a recorded terminal failure.

The pre-daemon bootstrap lease and append-only attempt journal live only under the private runtime directory; they never
mutate Fabric SQLite or create a second transaction owner. Socket absence or PID inspection alone never grants election.
A contender may reclaim only after lease expiry and release of the exclusive lock. The winner rechecks the socket while
holding the lock and retains the lease until the daemon owns the canonical socket and database, finishes
migration/recovery and publishes an atomic ready receipt. A live but incompatible incumbent that still owns the
canonical locks produces a typed compatibility failure, never a second daemon or socket replacement. Losers poll the
exact election generation or its bounded terminal result. Runtime directories remain `0700`, socket and lease material
private, and no project-session record may be created before initialisation succeeds.

A compatible incumbent handshake precedes database inspection and returns an attached client immediately. With no
compatible incumbent, the caller performs a mutation-free inspection before creating bootstrap artifacts, then repeats
the inspection while holding the winning election lock immediately before spawn. This ordering permits attachment to a
legitimate busy WAL writer while retaining byte/mode/directory preservation for incompatible state and closing the
absent-to-incompatible publication race.

Election and shutdown use the same lock order: acquire the daemon-election lock first, then begin the SQLite
liveness/recovery transaction. The daemon imports the winning bootstrap receipt into its audit journal only after it is
the sole database owner. Shutdown holds the election lock through its final liveness recheck and socket close, so
attach/start cannot race quiesce into a duplicate owner.

## Global liveness and idle stop

While holding the daemon-election lock, the daemon shall stop only after one SQLite transaction proves there is no
liveness-contributing project session or coordination run, active current-generation task/agent lease, unresolved
provider action, unresolved operator-effect custody or unexpired current-generation operator client. Required result
delivery remains a project-session closure blocker. Pending best-effort notification delivery alone does not keep the
daemon alive. An attached Console intentionally keeps it alive. Closing the final Console permits, but does not force,
idle shutdown.

Liveness-contributing project-session states are `awaiting_launch`, `launching`, `active`, `quiescing`,
`awaiting_acceptance`, `launch_ambiguous`, `reconciling`, `visibility_degraded`, `recovery_required` and `quarantined`.
Detached `draft`, `closed`, `cancelled` and explicitly terminalised `launch_failed` sessions do not keep the process
alive. Run liveness uses the corresponding launching/active/quiescing/acceptance, ambiguity, recovery and quarantine
states; draft and terminal closed/cancelled/failed history does not. Provider actions contribute only while `prepared`,
`dispatched`, `accepted`, `ambiguous` or `quarantined`. Historical terminal rows never block shutdown. Generic
`operator_effect_custody` contributes while `prepared`, `dispatching`, `ambiguous` or `failed`; `terminal`, `no-effect`
and `rejected` do not. A typed launch, Git, bridge-recovery, registered external-effect or Herdr owner remains the sole
recovery owner for its joined generic row; liveness may count that row only once, but it may never omit it because
another projection also blocks. Typed Git custody contributes while its four-owner mapping is `prepared`, `dispatching`,
`conflict`, `ambiguous` or `quarantined`; an operation draft alone never contributes. Machine or human terminalisation
removes only the mapped custody/reservation contribution.

Operator attachment is a persisted generation-fenced lease with heartbeat and bounded crash expiry. Stop uses a
daemon-instance compare-and-set transition from `running` to `quiescing`, records the observed global-state revision and
rechecks the idle predicate before closing the socket. A concurrent attach, project launch or recovered active member
cancels the quiesce or advances the revision so the stop fails closed. Project close and client detach are idempotent
and cannot stop another project's work.

## Current baseline and invariants

The single current baseline shall create:

- project sessions and explicit membership;
- coordination-run project/session links, lifecycle revision and chair generation, plus persisted delivery workstreams;
- operator principals, capabilities, client attachments, input attestations and idempotent commands;
- revisioned intakes, scoped gates and gate-to-task/operation/barrier links;
- hierarchical resource scopes and reservations;
- request-result delivery and transactional outbox state;
- attention items and notification delivery journal;
- daemon runtime epochs and current bootstrap audit receipts; and
- one active MCP seat generation per project plus its exact session/run/chair, roster, principal-generation and token
  bindings; and
- immutable artifact-publication lineage, review targets/bundles, provider-action route/result custody and terminal
  review evidence; and
- schema-versioned operator projection cursors.

The baseline installs same-project/run foreign-key and enumeration/generation triggers plus indexes for active
membership, gate enforcement, intake revision, callback deadline/claim, resource admission, notification dedupe and
global-idle queries. It is created only for an absent database path and is verified before atomic publication.

MCP roster rotation is one prepare/activate compare-and-swap. The caller supplies the expected active generation and a
content-derived immutable next generation. Activation rechecks the current project session, run, chair lease, revisions
and every principal, supersedes the prior generation and revokes its seat tokens in the same transaction. Initialisation
and every later capability use join through the one active seat generation; a stale token or manually rolled-back
filesystem pointer therefore fails closed. Filesystem staging holds one private project lock, publishes only when
`current.json` equals the expected prior generation (or already equals the exact replay), and never replaces a newer
pointer. Interrupted prepare/activation is recoverable without reviving a superseded roster.

The baseline includes the append-only launch-attempt owner. Its closed logical shape is:

```sql
CREATE TABLE project_session_launch_custody (
  project_session_id TEXT NOT NULL,
  attempt_generation INTEGER NOT NULL CHECK (attempt_generation >= 1),
  run_id TEXT NOT NULL UNIQUE,
  chair_agent_id TEXT NOT NULL,
  provider_adapter_id TEXT NOT NULL,
  provider_action_id TEXT NOT NULL,
  operator_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  capability_token_hash TEXT NOT NULL UNIQUE,
  resource_reservation_id TEXT NOT NULL UNIQUE,
  launch_packet_path TEXT NOT NULL,
  launch_packet_digest TEXT NOT NULL,
  resource_plan_path TEXT NOT NULL,
  resource_plan_digest TEXT NOT NULL,
  expected_project_revision INTEGER NOT NULL
    CHECK (expected_project_revision >= 1),
  prepared_from_session_revision INTEGER NOT NULL
    CHECK (prepared_from_session_revision >= 1),
  session_generation INTEGER NOT NULL CHECK (session_generation >= 1),
  trust_record_digest TEXT NOT NULL,
  adapter_contract_digest TEXT NOT NULL,
  resource_state_digest TEXT NOT NULL,
  launch_binding_digest TEXT NOT NULL,
  retry_of_adapter_id TEXT,
  retry_of_action_id TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (project_session_id, attempt_generation),
  UNIQUE (provider_adapter_id, provider_action_id),
  UNIQUE (operator_id, command_id),
  CHECK ((retry_of_adapter_id IS NULL) = (retry_of_action_id IS NULL)),
  FOREIGN KEY (project_session_id, run_id)
    REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY (run_id, chair_agent_id)
    REFERENCES agents(run_id, agent_id),
  FOREIGN KEY (provider_adapter_id, provider_action_id)
    REFERENCES provider_actions(adapter_id, action_id),
  FOREIGN KEY (operator_id, command_id)
    REFERENCES operator_commands(operator_id, command_id),
  FOREIGN KEY (capability_token_hash) REFERENCES capabilities(token_hash),
  FOREIGN KEY (resource_reservation_id)
    REFERENCES resource_reservations(reservation_id),
  FOREIGN KEY (retry_of_adapter_id, retry_of_action_id)
    REFERENCES project_session_launch_custody(
      provider_adapter_id, provider_action_id
    )
);
```

The baseline also adds positive `journal_revision` and nullable `outcome_digest` columns to `provider_actions`. Every
state or outcome CAS increments the revision; a non-null digest hashes the canonical closed outcome. This is the source
of `LaunchProviderActionJournalRefV1`, not an artifact projection. Its nested action reference equality-binds the
canonical ProviderActionRefV1 pair. The baseline creates the parent unique index before the custody table and a trigger
that also requires the referenced provider action's `run_id` to equal the custody `run_id`. It preflights and enforces
daemon-global `provider_actions(adapter_id, action_id)` uniqueness across runs; the core and every adapter journal key
the same pair. Duplicate existing pairs fail preflight before mutation. Attempt generations are contiguous per session,
and a retry must reference that session's immediately prior proved-failed attempt. Update and delete triggers make
custody rows immutable; provider-action and lifecycle rows own outcome. No run or session is imported from an earlier
database epoch.

## Restart and ambiguous-effect recovery

Under the daemon-election lock and before opening SQLite, startup reconciles an expired runtime bootstrap lease and
records its terminal attempt receipt. It does not treat that lease as database state. An unclean marker then triggers
bounded integrity and foreign-key checks before mutation. Startup then, transactionally:

- restores project-session and operator projection revisions;
- expires only operator attachments whose deadline and daemon-generation predicates are proven;
- requeues unacknowledged mailbox deliveries under their existing rules;
- returns only an expired `claimed` result-delivery lease to `pending` with a higher claim generation;
- preserves `provider-accepted`, `overdue`, `abandoned` and `consumed` result deliveries without regression, reinjection
  or reassignment;
- marks expired response deadlines `overdue` without redispatch;
- resumes notification attempts from their durable dedupe keys; and
- assigns every unresolved generic operator-effect custody to exactly one recovery owner: an unowned `prepared` row may
  become `no-effect` only after proving dispatch never began; `dispatching`, `ambiguous` and `failed` use lookup/observe
  only, never redispatch, and remain blocking when no complete proof is available; and
- quarantines ambiguous provider, Git, Herdr or notification effects until lookup/reconciliation proves their outcome.

No pane, process absence or Console-local cache may infer coordination state. The Console may rebuild its complete
projection from an authoritative snapshot plus monotonic event cursor after any restart.

Generic provider-action recovery shall exclude every row joined to `project_session_launch_custody`, regardless of
provider state. Launch recovery uses the immutable custody owner and applies this closed policy:

- `prepared`: make no adapter call, revoke the chair capability hash and chair lease, release the resource reservation,
  terminalise the run/action and CAS the session to `launch_failed`;
- `dispatched`, `accepted` or `ambiguous`: call only the adapter's pair-keyed `lookup_action`; never dispatch, replay or
  reconstruct launch material; and
- a strict `terminal-success` with an exact resume reference lets the internal reconciler activate and settle resources
  once; strict `terminal-no-effect` proof lets it fail and clean up; every absent, error, malformed, incomplete or
  conflicting result remains `launch_ambiguous` with its run, lease, hash and reservation.

Recovery never derives plaintext from a hash or durable payload. It performs the prepared cleanup or lookup before
admitting a retry or idle shutdown. A public transition or `operatorActionReconcile` request cannot invoke this path.

## Notification worker

The daemon-owned notification worker consumes durable attention items while project work remains active. Notification
attempts are best-effort and non-authoritative. Each has a stable dedupe key, target integration, exact item revision
and state `pending`, `claimed`, `sent`, `failed`, `deduplicated` or `ambiguous`; integration availability is separately
`available`, `unavailable` or `stale`. Retries append attempts and never approve, acknowledge or consume the attention
item. A crash after claim but before terminal journalling records ambiguity and never blindly retries. An exact focus
action is emitted only for an integration whose discovered contract advertises a tested link/action capability.

## Verification additions

Acceptance requires deterministic tests for:

- simultaneous first reads producing one daemon and one socket owner;
- stale/unreachable sockets, bounded initialisation timeout and incompatible handshakes without duplicate spawn or
  socket replacement;
- crash at every bootstrap phase and safe stale-lease reconciliation;
- two projects launching/closing while clients attach/detach without premature shutdown;
- restart through every project-session and result-delivery state;
- migration preflight, rollback, trigger and query-plan enforcement;
- global-idle false positives for every liveness predicate;
- every unresolved generic operator-effect state blocking idle stop and project-session closure until its exact recovery
  owner proves a terminal outcome, including an unknown/missing owner failing closed;
- detached draft sessions and terminal historical rows not blocking idle stop;
- election racing quiesce/stop through the canonical lock order;
- Console crash/restart without task cancellation or duplicate commands;
- notification dedupe, restart, unavailable/stale labelling and non-authoritative action handling; and
- deterministic projection snapshot plus cursor replay.

Load evidence shall cover concurrent session membership, scoped-gate reads, budget admission, result callbacks and
operator projection alongside the existing 32-agent/1,000-operation coordination mix.

## Private project/operator provisioning and launch custody

The daemon's current private-control connection owns the bootstrap method, `provisionLocalOperator`. It is available
only after the normal daemon initialisation handshake with the current private bootstrap capability. The method rechecks
an exact canonical root and trust-record digest, derives the local subject binding, and transactionally creates or
revalidates the project, operator principal and bounded project capability described by the launch-resource contract.
`projects.canonical_root` plus non-null `projects.trust_record_digest` own that current binding. It cannot widen an
existing project/root binding. Exact replay is idempotent; changed input or stale generation fails closed.
`OperatorStore.rotatePrincipal` is the only rotation surface: it compare-and-sets and increments `principal_generation`
and revokes every older capability in the same transaction. The returned plaintext token is a one-time local handoff and
is never placed in the daemon discovery receipt or durable audit. Revocation and later bounded issuance use the same
generation fences.

After the public `projectSessionCreate` call has committed a draft, the private method
`issueLocalOperatorSessionCapability` rechecks the local subject, project/trust binding, session generation and
requested action subset. It returns a session-bound token whose expiry is no later than the project capability and
reviewed launch-envelope expiry. A `project-launch` capability remains forbidden for session-targeted commands, and
neither public creation nor projections return credentials.

## Closed launch inspection and private effect boundary

The public operator connection may create a draft session and prepare `draft -> awaiting_launch`; preparation requires
the reviewed packet reference, atomically replaces the session packet path/digest and increments its revision. The
transition request carries that packet reference only for this preparation. The public path shall reject every request
to enter or leave `launching`, enter or leave `launch_ambiguous` or reconcile a launch provider action. Chair launch
occurs only through preview/commit of the strict `ProjectSessionLaunchIntent`, `launch_packet_v1` and
`launch_resource_plan_v1` contracts in the launch-resource contract.

Before preview, the daemon parses both artifacts with closed schemas, resolves all paths from the trusted project root
and validates provider input through the exact registered adapter contract digest. Inspection normalises the chair
authority and computes the project revision, session revision/generation, trust-record digest, adapter-contract digest,
resource-state digest and one canonical launch-binding digest. It cross-checks packet, plan, intent, stored identity,
topology, budget, resource hierarchy and provider action. Preview persists every binding. Commit repeats inspection and
requires byte-identical canonical bindings; stale or changed state has zero effect. An initial preview requires the
stored packet to equal the proposed reference. A retry preview instead binds both the prior failed-attempt packet and a
newly reviewed proposed packet.

One daemon-owned `LaunchCustodyService` has four operations:

1. `inspect(intent)` safely reads, parses, normalises and binds current state;
2. `prepareInTransaction(...)` executes synchronously inside the operator commit transaction and returns only a volatile
   post-commit dispatch handle;
3. `dispatchPrepared(handle)` persists `dispatched` before adapter I/O and invokes the dedicated secret-bearing adapter
   handoff; and
4. `recover()` applies prepared cleanup or observe-only lookup under the rules in the daemon recovery rules.

The generic operator effect port and generic provider startup path never own a launch step. The operator command journal
records preparation and the stable provider adapter/action pair. Pending, ambiguous and terminal `OperatorActionStatus`
values and the receipt project the provider-action journal without rewriting the original command's before/after audit.

The service generates the chair credential with a cryptographically secure random source and stores only its hash.
Plaintext exists once in the volatile post-commit handle. A dedicated versioned adapter method consumes that handle at
most once to configure the chair's local Fabric access. The secret is a separate argument from the persisted, strict
public launch payload; it is never prompt/model input, provider payload/history, operator JSON, event, discovery
material, log or error detail. Adapter implementations shall redact the secret before propagating any failure.

The provider session, not an adapter-local probe, supplies continuity evidence. Launch custody creates a 32-byte one-use
random challenge, persists only its digest and sends the raw value only in the volatile private handoff. The exact
session must echo it in a native provider tool invocation. The provider contract declares its invocation-attribution
mechanism; the shipped adapter returns the bounded provider-emitted session, turn and call identifiers plus response and
launch bindings. The daemon verifies them against custody before accepting the canonical non-secret attestation digest.
The adapter is the trusted translation boundary, so conformance exercises its real code against a fake native provider
transport; adapter-local invocation with no provider event is rejected. Neither challenge nor attestation may carry the
credential.

On success the supervisor retains the owning adapter/session bridge for later turns. The adapter may keep the credential
only in volatile bridge state and must not redisclose it or place it in model input/history. Pre-terminal bridge loss is
ambiguous. Post-terminal bridge loss is explicit context/chair loss: the persisted resume reference alone cannot
recreate continuity, and recovery must fence or take over the chair under the existing generation/handoff rules.
Providers that cannot originate the attestation remain unproved and cannot produce terminal success.

Dispatch return and lookup both parse the closed `launch_adapter_outcome_v1` union from the Agent Fabric contract. Only `terminal-success`
with the exact action pair, usable resume reference, positive provider generation and complete per-reservation usage may
activate. Only `terminal-no-effect` with a contract-validated proof may fail cleanly. Accepted-only, absent, error,
malformed, incomplete or conflicting evidence becomes `ambiguous`. Launch status and receipts expose the typed
`LaunchProviderActionJournalRefV1`; they never fabricate an effect artifact to represent journal state.

## Atomic custody, outcomes and retry

For an initial attempt, `prepareInTransaction` CASes `awaiting_launch` to `launching`. For a retry, it CASes
`launch_failed` to `launching`, replaces the session packet path/digest with the newly reviewed reference and increments
the session revision. It then atomically creates or revalidates all of these rows: coordination run; narrowed authority
and budget; exactly one chair; random capability hash; chair lease and mailbox; adapter binding; project/session/run
resource scopes and dimensions; a launch reservation whose daemon-derived ID and `operation_id` bind the canonical
provider adapter/action pair; prepared provider action; immutable custody ownership; required project-session
memberships; and the operator preparation. Every topology, revision, generation, trust, resource and idempotency
predicate is rechecked inside the write transaction. Rollback retains the prior packet reference.

The custody row never stores outcome or plaintext and is never updated. Its session attempt generation, run, chair,
command, provider pair, capability hash, reservation and binding digests permanently identify the attempt.
`provider_actions` owns progress and outcome. The daemon and every adapter journal enforce the same global `(adapter_id,
action_id)` identity, including cross-run use.

The internal custody reconciler alone CASes `launching` to `active`, `launch_failed` or `launch_ambiguous` from
persisted provider evidence. A `terminal-no-effect` result revokes the capability hash and chair lease, releases the
reservation and terminalises the run/action. An ambiguous result retains its run, lease, hash, reservation and action
identity and permits lookup only. Neither restart nor duplicate commit dispatches it again.

For `terminal-success`, the active-state CAS and reservation reconciliation are one transaction with persistence of the
exact resume reference and provider generation. Exact usage consumes each reported amount and releases the remainder. An
`unknown` dimension marks every affected ancestor unknown and closes the reservation without restoring unproved
capacity. Usage above the reservation enters `recovery_required` rather than truncating evidence. A `terminal-no-effect`
result releases every reserved unit; ambiguity settles nothing. A successful launch proves exactly one `provider_calls`
unit and zero retained `concurrent_turns` units; dimensions not proved by terminal evidence remain unknown. Confirmed
chair-abandon may reconcile only those facts when exact terminal-success action and loss-bound custody match; historical rows are not rewritten and every other genuinely unknown dimension stays unknown.

`launch_ambiguous` prohibits a retry or second chair. After lookup proves failure, retry requires a fresh current-state
preview, newly reviewed packet, new run ID, new provider adapter/action pair, next custody attempt generation and exact
`retry_of` binding to the failed row. Its commit atomically replaces the session packet reference; no public transition
performs that launch-owned CAS. An exact duplicate commit returns the existing public result without another transaction
effect or adapter call; a changed replay conflicts. Failed and ambiguous custody rows remain immutable.

## Launch-custody verification additions

Deterministic fake-adapter and crash-injection gates shall prove:

- a fresh `decide`/transition command cannot enter or leave launch-owned state, and leaves zero run, action, lease,
  reservation or custody rows;
- a fault after every preparation statement rolls back every launch-owned row;
- a crash after prepared commit and before dispatch makes zero adapter calls on restart, revokes the capability hash and
  chair lease, releases the reservation and records proved failure;
- a crash after persisted `dispatched`, before or during adapter I/O, performs pair-keyed lookup only on restart;
- provider acceptance before core outcome persistence activates the same run exactly once after lookup only when
  terminal success includes the exact resume reference and provider generation;
- wrapper-only probes, wrong-session or replayed challenges and a bridge closed at launch return cannot produce terminal
  success; the exact provider session must originate attestation and remain reachable through its owning bridge;
- accepted-only, missing-resume, absent, error, malformed and conflicting lookup fixtures remain ambiguous, while a
  contract-valid no-effect proof alone produces failed cleanup;
- two concurrent coordinated commits produce at most one non-terminal run, chair and provider action;
- an exact duplicate commit dispatches once, while a changed replay conflicts;
- packet, plan, project/session revision, trust record, adapter contract or resource state change after preview rejects
  with zero effect;
- unknown/extra artifact fields, symlink escape, authority widening and forbidden provider controls reject before
  persistence;
- cross-run reuse of one adapter/action pair fails before adapter I/O, including migration preflight and adapter-journal
  coverage;
- an ambiguous launch retains the run, lease, capability hash and reservation, and cannot retry or create another chair;
- terminal success consumes exact reported usage, releases its remainder and propagates unknown dimensions without
  restoring unproved capacity; no-effect failure releases the full reservation;
- pending, ambiguous and terminal launch projections preserve the exact typed provider-action reference without a
  synthetic effect artifact;
- a secret canary is absent from protocol responses, previews, projections, commands, provider payload/history, events,
  receipts, discovery material, logs and adapter errors;
- a proved-failure retry requires a new reviewed packet, run, action pair and incremented custody attempt generation
  with the exact prior binding, and its packet replacement rolls back with any failed commit; and
- public `operatorActionReconcile` rejects launch, while internal custody lookup alone may resolve it.

These gates also cover private/public principal separation, trust-root recheck, duplicate local provisioning and daemon
restart without blind chair respawn.

## Schema-derived MCP and provider-session tool projection

One daemon-owned authenticated agent protocol is the transport authority for both standalone MCP proxies and retained
launched-chair bridges. MCP tool descriptors are generated from the exhaustive `tool` classifications in the active
agent-principal operation registry and the protocol's closed input/output codecs. The registry is the only
membership/name owner; documentation and provider projections consume its generated artifact. Startup negotiates the
current feature and operation grant before `tools/list`; a stale descriptor, missing feature or revoked generation is
removed or rejected before daemon mutation. The current private-control method vocabulary cannot own the MCP tool list
or bypass public principal/generation checks.

`bindCurrentMcpSeats` requires the exact expected prior generation and a content-addressed immutable replacement. The
database persists immutable generation/member rows, owns one active generation per project and revokes the prior member
capabilities in the activation transaction. Exact replay of the active generation is safe; reuse of a retired
generation, a stale predecessor or a crossed project/session/run binding is rejected. Filesystem publication stages and
fsyncs a complete immutable generation, then under an OS-backed per-project lock compare-and-swaps `current.json`,
including its predecessor. Interrupted staging leaves the prior complete pointer active and a delayed old writer cannot
restore an earlier generation. No flat-file or prior-generation authentication fallback exists.

The standalone proxy accepts only an `afc_` agent capability and requests an agent principal. A bootstrap `afb_`
credential is rejected before `tools/list`; there is no private run-creation descriptor. Every active agent operation is
classified `tool` or `none`, and the build fails for an absent or stale classification. V1 projects exactly one complete
descriptor per operation and permits no constant-bound aliases. Secret-bearing `registerAgent` and `rotateCapability`
remain `none`.

Provider adapters project the same descriptors into their supported native tool mechanism. The Claude SDK bridge owns
one in-process MCP server per live chair session. The Codex app-server bridge owns dynamic tools on the exact retained
thread/connection. Both validate the provider-emitted invocation, closed arguments and exact closed daemon result; both
retain only volatile credential and transport state. The attestation challenge is an additional private tool and is not
a substitute for the coordination surface. It remains a registry-owned `tool` operation, projected only by the
launch-attestation feature/grant and excluded from standalone proxies. Successful launch retains the adapter
process/connection so a later provider turn can call a normal Fabric tool. Release or supervisor shutdown closes it
once. Unexpected loss before terminal launch is ambiguous; loss after activation journals provider- context/chair loss
and fences normal delivery/turn authority.

Spawn and attach use the existing custody owner rather than a parallel secret path. The daemon prepares the target
capability hash and stable provider action before I/O. A bridge-capable adapter consumes plaintext once through private
volatile handoff, journals the stable action and retains a generation-bound bridge; lookup alone resolves ambiguity
after restart. Public protocol and MCP results replace the token with target identity plus `bridgeState` and
`bridgeGeneration`. An adapter without bridge provisioning reports that closed capability before dispatch; attach may
remain an honest bridge-less participant, but no surface claims provider-originated Fabric access. Raw adapter result
JSON is never model-visible: the public codec exposes only typed contract evidence and canonical digests. A non-review
task-bound ephemeral action may retain its validated bounded `providerAnswer`; a certifying review exposes only answer
digest and safe parsed result. Every variant has `additionalProperties: false`.

The hard projection limits are 96 tools, 32 KiB canonical JSON per descriptor and 512 KiB for the complete descriptor
set. The complete authorised set must fit and match across projections; exceeding any bound rejects MCP connection or
chair launch with the exact excess descriptor names. It is never truncated. Arguments/results use the negotiated 1 MiB
frame, 32 pending-call, 16 in-flight, 30-second request and five-minute idle maxima. Idle bounds an unused connection, not
a provider turn: a terminal idle transport reconnects and replays the next, never-submitted operation. A typed local queued
timeout is also never submitted and receives one bounded, same-principal replay. In-flight loss replays only a command ID
or message-send dedupe key; other ambiguous operations reconnect with typed guidance, never raw `PROTOCOL_TIMEOUT`. An
ambiguous receive stays unknown and unacknowledged; retry waits its visibility timeout. Runtime never exposes raw transport failure.

Deterministic acceptance adds:

- generated descriptor parity against every active the Console contract agent operation and negative drift fixtures for an
  added/removed/unclassified operation or feature;
- bootstrap-credential rejection before tool advertisement; no `fabric_run_create`; exhaustive `tool`/`none`
  classification and no copied or constant-bound alias descriptors;
- codec-wide secret scans plus spawn/attach custody tests proving secret-free public/MCP results, exact bridge-state
  honesty, supported later-turn calls and unsupported bridge absence without fabricated continuity;
- identical tool names and closed schemas across standalone Claude/Codex MCP, Claude SDK MCP and Codex dynamic-tool
  projections;
- point-of-use wrong-run, wrong-chair, wrong-session-generation, revoked, expired and action-insufficient rejection
  through every projection;
- provider-originated attestation followed by a distinct later-turn mailbox or coordination call over the same bridge,
  with zero wrapper self-probe path;
- bridge/proxy crash, timeout, malformed argument/result, oversize, duplicate and concurrent-call coverage without
  daemon loss, duplicate external effect or false task/message completion; and
- end-to-end MCP proxy and production Console/TUI dogfood against one real elected daemon and canonical socket;
  real-provider later-turn dogfood uses an already authenticated installation only when the run has explicit provider
  authority, performs no login or persistent MCP registration, and otherwise records a non-passing `not-run` gate rather
  than substituting a fake.

## Bridge-loss recovery and lifecycle retirement

Startup and live adapter supervision compare every active retained chair and child bridge with the volatile
retained-bridge registry. A missing/closed chair bridge atomically persists one immutable loss row under the daemon
generation, freezes the old chair lease/delivery/grants, revokes the old capability and CASes the run and session to
`recovery_required`. The recovery manifest hashes current task, mailbox, lease, checkpoint, membership, provider and
revision facts. Repeated observation is idempotent; absence of a bridge is never inferred as provider death or a safe
retry.

A missing/closed non-chair child bridge instead persists one immutable child loss, revokes its exact capability,
advances the bridge generation and projects `lost`/`none` without forcing the run or project session into
`recovery_required`. Repeated detection is idempotent; the dead transport cannot authenticate or replay a provider call.
Chair-only recovery custody below does not silently rebind a child.

An operator-authorised recovery custody row binds the loss, manifest, selected path, expected generations/revisions,
adapter contract and stable action ID. `rebind` prepares a new hash-only capability and daemon challenge under a higher
generation, then dispatches once to the dedicated adapter reattach operation. The same provider resume reference is
context only: fresh native tool-call attestation is mandatory. `takeover` requires a named successor whose live bridge
and narrowed authority are current. `abandon` records the terminal loss path. Prepared restart performs zero adapter
I/O; dispatched/accepted/ambiguous restart performs pair-keyed lookup only. Terminal success atomically activates the
new generation and retains its bridge. Failure cleans only proved no-effect; ambiguity retains custody and remains
fenced.

The local Console obtains takeover authority only from the daemon's exact unresolved-loss lookup; it cannot supply a
loss ID or author a recovery intent. The issued intent is bound to that loss, manifest, run, session and bridge
generation. Chair abandon remains destructive and therefore follows the normal Review then explicit Confirm path.

The four direct lifecycle protocol operations are retired and never granted. Only `OperatorActionIntent` lifecycle
variants may reach production state/effect ports because they bind preview, exact global state, session/run generation,
consequence evidence and confirmation. Compatibility decoders may explain the replacement but cannot capture a current
revision or execute.

Verification adds crash points before/after every loss/recovery statement, daemon restart with an active launched chair,
same-action lookup recovery, wrong loss/manifest/generation, stale resume reference, missing native callback,
successor-without-live-bridge, explicit abandon and duplicate observation. It also asserts the direct lifecycle
operations are absent from every grant/client and return retirement errors if sent manually. Child-bridge coverage drops
a post-activation transport and proves loss persistence, capability revocation, generation advance, honest projection
and later-call rejection without chair-level run fencing.

## Terminal results, evidence heads, and route recovery

### Terminal results and linear evidence heads

provider_review_results is insert-only and has one closed discriminator:

- safe-answer: exact provider-answer digest/length, safe canonical review-result.v1, result/finding/resolved-finding
  digests, classifier and secret-selector identity;
- unusable-answer: exact provider-answer digest/length, safety identity and no public text/result/findings; or
- provider-terminal-failure: exactly one of max-turns-exhausted, provider-rejected, terminal-no-answer or
  adapter-terminal-failure code, private normalised diagnostic digest, no answer digest and no public error.

The joined public action terminal discriminator additionally admits terminal-no-effect, integrity-terminal and
retired-unknown from the route- integrity owner. These never create provider_review_evidence. ambiguous remains strictly
nonterminal; a terminal row cannot also project ambiguous.

A terminal failure is terminal, not ambiguous. Every proved-effect terminal -- safe answer, unusable answer or
provider-terminal-failure -- settles complete authenticated usage exactly. If usage is absent or partial, the same
transaction conservatively consumes the full remaining spendable reservation. Each releases terminal concurrency
capacity. Proved no-effect releases the reservation; ambiguity retains it. No retry or redispatch occurs. Raw answers,
raw errors, diagnostics and adapter results stay private. result_digest uses the exact six-arm Agent Fabric contract canonical domain,
including the stable run terminal sequence and coverage-summary digest where applicable, and excludes usage. Generated
golden vectors reject generic terminal-state or cross-arm fields.

A safe answer becomes certifying only when the trusted journal covers the mandatory set including every deterministic
risk sample and hashes to read_coverage_digest. With insufficient reads, syntactic CLEAN is publicly UNUSABLE and
resolves nothing; safely parsed FINDINGS stays visible FINDINGS/noncertifying, accepts no resolution and retains all
safe new findings. Raw unsafe output is UNUSABLE. Provider text cannot attest consumption. The daemon derives a
manifest-complete-risk-directed gap summary with per-group total/read/ unread counts and unread-set digests;
byteComplete is false unless every object was fully read.

The proved actual-route identity is a dedicated immutable child of the exact admission observation. A digest cannot be
coined directly on review evidence. The full current-baseline result relation is reproduced below rather than replaced
by a thinner parent stub. It retains every result field and arm CHECK; the only extension is the journal-bound terminal
sequence and composite evidence-parent key.

~~~sql
provider_action_actual_route_identities(
  adapter_id NOT NULL, action_id NOT NULL,
  admission_digest NOT NULL, observation_digest NOT NULL,
  actual_route_identity_json NOT NULL,
  actual_route_identity_digest NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(adapter_id,action_id,admission_digest,observation_digest,
    actual_route_identity_digest),
  FOREIGN KEY(adapter_id,action_id,admission_digest,observation_digest)
    REFERENCES provider_action_route_observations(
      adapter_id,action_id,admission_digest,observation_digest)
)

provider_review_terminal_journal(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  target_generation INTEGER NOT NULL,
  slot TEXT NOT NULL,
  attempt_generation INTEGER NOT NULL CHECK(attempt_generation >= 1),
  terminal_kind TEXT NOT NULL CHECK(terminal_kind IN
    ('safe-answer','unusable-answer','provider-terminal-failure',
      'terminal-no-effect','integrity-terminal','retired-unknown')),
  terminal_sequence INTEGER NOT NULL CHECK(terminal_sequence >= 1),
  terminal_input_digest TEXT NOT NULL,
  private_answer_digest TEXT,
  private_result_digest TEXT,
  private_adapter_result_digest TEXT,
  authenticated_usage_digest TEXT,
  read_journal_digest TEXT,
  public_terminal_projection_digest TEXT NOT NULL,
  evidence_mutation_receipt_digest TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(adapter_id,action_id,target_generation,slot,attempt_generation),
  UNIQUE(run_id,terminal_sequence),
  UNIQUE(adapter_id,action_id,terminal_sequence,terminal_kind),
  FOREIGN KEY(adapter_id,action_id,run_id,target_generation,slot,
      attempt_generation)
    REFERENCES provider_action_routes(
      adapter_id,action_id,run_id,target_generation,slot,attempt_generation)
)

provider_review_results(
  adapter_id TEXT NOT NULL,
  action_id TEXT NOT NULL,
  terminal_sequence INTEGER NOT NULL CHECK(terminal_sequence >= 1),
  result_kind TEXT NOT NULL CHECK(result_kind IN
    ('safe-answer','unusable-answer','provider-terminal-failure')),
  provider_answer_digest TEXT,
  provider_answer_length INTEGER CHECK(
    provider_answer_length IS NULL OR provider_answer_length >= 0),
  safe_result_json TEXT CHECK(
    safe_result_json IS NULL OR json_valid(safe_result_json)),
  result_digest TEXT NOT NULL UNIQUE,
  finding_set_digest TEXT,
  resolved_finding_set_digest TEXT,
  classifier_digest TEXT,
  secret_selector_digest TEXT,
  failure_code TEXT CHECK(failure_code IS NULL OR failure_code IN
    ('max-turns-exhausted','provider-rejected','terminal-no-answer',
      'adapter-terminal-failure')),
  private_diagnostic_digest TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(adapter_id,action_id,terminal_sequence,result_kind,
    provider_answer_digest,result_digest),
  FOREIGN KEY(adapter_id,action_id,terminal_sequence,result_kind)
    REFERENCES provider_review_terminal_journal(
      adapter_id,action_id,terminal_sequence,terminal_kind),
  CHECK((result_kind='safe-answer' AND provider_answer_digest IS NOT NULL AND
      provider_answer_length IS NOT NULL AND safe_result_json IS NOT NULL AND
      finding_set_digest IS NOT NULL AND
      resolved_finding_set_digest IS NOT NULL AND
      classifier_digest IS NOT NULL AND secret_selector_digest IS NOT NULL AND
      failure_code IS NULL AND private_diagnostic_digest IS NULL) OR
    (result_kind='unusable-answer' AND provider_answer_digest IS NOT NULL AND
      provider_answer_length IS NOT NULL AND safe_result_json IS NULL AND
      finding_set_digest IS NULL AND resolved_finding_set_digest IS NULL AND
      classifier_digest IS NOT NULL AND secret_selector_digest IS NOT NULL AND
      failure_code IS NULL AND private_diagnostic_digest IS NULL) OR
    (result_kind='provider-terminal-failure' AND
      provider_answer_digest IS NULL AND provider_answer_length IS NULL AND
      safe_result_json IS NULL AND finding_set_digest IS NULL AND
      resolved_finding_set_digest IS NULL AND classifier_digest IS NULL AND
      secret_selector_digest IS NULL AND failure_code IS NOT NULL AND
      private_diagnostic_digest IS NOT NULL))
)

provider_review_evidence(
  run_id NOT NULL, evidence_id NOT NULL,
  target_generation NOT NULL CHECK(target_generation >= 1),
  slot NOT NULL CHECK(slot IN
    ('native','other-primary','cursor-grok','agy-gemini')),
  task_id NOT NULL,
  action_adapter_id NOT NULL, action_id NOT NULL,
  terminal_sequence NOT NULL CHECK(terminal_sequence >= 1),
  terminal_kind NOT NULL CHECK(terminal_kind IN
    ('safe-answer','unusable-answer')),
  verdict NOT NULL CHECK(verdict IN ('CLEAN','FINDINGS','UNUSABLE')),
  answer_safety NOT NULL CHECK(answer_safety IN ('safe','unusable')),
  provider_answer_digest NOT NULL, terminal_result_digest NOT NULL,
  review_result_digest,
  route_receipt_digest NOT NULL, route_admission_digest NOT NULL,
  route_observation_digest, actual_route_identity_digest,
  final_prompt_digest NOT NULL,
  endpoint_provider NOT NULL, provider_family NOT NULL, model NOT NULL,
  bundle_digest NOT NULL, coverage_digest NOT NULL, profile_digest NOT NULL,
  chair_binding_generation NOT NULL CHECK(chair_binding_generation >= 1),
  chair_binding_digest NOT NULL,
  prior_head_generation NOT NULL CHECK(prior_head_generation >= 0),
  new_head_generation NOT NULL CHECK(new_head_generation >= 1),
  attempt_generation NOT NULL CHECK(attempt_generation >= 1),
  prior_evidence_id,
  prior_open_finding_set_digest NOT NULL,
  reported_resolved_finding_set_digest NOT NULL,
  accepted_resolved_finding_set_digest NOT NULL,
  finding_set_digest NOT NULL, new_open_finding_set_digest NOT NULL,
  repair_required_finding_set_digest NOT NULL,
  finding_window_digest NOT NULL,
  finding_capacity_reservation_digest NOT NULL,
  read_coverage_digest NOT NULL, coverage_summary_digest NOT NULL,
  reviewer_family_relation NOT NULL CHECK(reviewer_family_relation IN
    ('same-family-exempt','distinct-family-proved','same-family-forbidden',
      'family-unproved')),
  certification_basis_at_terminal_digest NOT NULL,
  mutation_receipt_digest NOT NULL,
  evidence_json NOT NULL, evidence_digest NOT NULL, created_at NOT NULL,
  PRIMARY KEY(run_id,evidence_id),
  UNIQUE(action_adapter_id,action_id),
  UNIQUE(evidence_digest), UNIQUE(mutation_receipt_digest),
  UNIQUE(run_id,target_generation,slot,evidence_id),
  UNIQUE(run_id,target_generation,slot,new_head_generation),
  UNIQUE(run_id,target_generation,slot,new_head_generation,evidence_id),
  CHECK(new_head_generation=prior_head_generation+1),
  CHECK((prior_head_generation=0)=(prior_evidence_id IS NULL)),
  CHECK((terminal_kind='safe-answer' AND answer_safety='safe' AND
      verdict IN ('CLEAN','FINDINGS') AND review_result_digest IS NOT NULL) OR
    (terminal_kind='unusable-answer' AND answer_safety='unusable' AND
      verdict='UNUSABLE' AND review_result_digest IS NULL)),
  CHECK(actual_route_identity_digest IS NULL OR
    route_observation_digest IS NOT NULL),
  FOREIGN KEY(action_adapter_id,action_id,terminal_sequence,
      terminal_kind,provider_answer_digest,terminal_result_digest)
    REFERENCES provider_review_results(
      adapter_id,action_id,terminal_sequence,result_kind,
      provider_answer_digest,result_digest),
  FOREIGN KEY(action_adapter_id,action_id,route_receipt_digest,
      route_admission_digest)
    REFERENCES provider_action_routes(
      adapter_id,action_id,route_receipt_digest,
      deployed_route_admission_digest),
  FOREIGN KEY(action_adapter_id,action_id,route_admission_digest,
      route_observation_digest)
    REFERENCES provider_action_route_observations(
      adapter_id,action_id,admission_digest,observation_digest),
  FOREIGN KEY(action_adapter_id,action_id,route_admission_digest,
      route_observation_digest,actual_route_identity_digest)
    REFERENCES provider_action_actual_route_identities(
      adapter_id,action_id,admission_digest,observation_digest,
      actual_route_identity_digest),
  FOREIGN KEY(action_adapter_id,action_id,run_id,target_generation,slot,
      attempt_generation,finding_capacity_reservation_digest)
    REFERENCES review_finding_capacity_reservations(
      adapter_id,action_id,run_id,target_generation,slot,
      attempt_generation,reservation_digest),
  FOREIGN KEY(run_id,target_generation,task_id,bundle_digest,coverage_digest,
      profile_digest)
    REFERENCES review_completion_targets(
      run_id,target_generation,task_id,bundle_digest,coverage_digest,
      resolved_profile_digest),
  FOREIGN KEY(run_id,target_generation,chair_binding_generation,
      chair_binding_digest,task_id,bundle_digest,profile_digest)
    REFERENCES review_target_chair_bindings(
      run_id,target_generation,binding_generation,binding_digest,task_id,
      bundle_digest,profile_digest),
  FOREIGN KEY(run_id,target_generation,slot,prior_head_generation,
      prior_evidence_id)
    REFERENCES provider_review_evidence(
      run_id,target_generation,slot,new_head_generation,evidence_id),
  FOREIGN KEY(prior_open_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY(reported_resolved_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY(accepted_resolved_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY(finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY(new_open_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY(repair_required_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest)
)
~~~

review_slot_heads is the sole linear current evidence owner:

~~~sql
review_slot_heads(
  run_id NOT NULL,
  target_generation NOT NULL CHECK(target_generation >= 1),
  slot NOT NULL CHECK(slot IN
    ('native','other-primary','cursor-grok','agy-gemini')),
  head_generation NOT NULL CHECK(head_generation >= 0), head_evidence_id,
  latest_attempt_generation NOT NULL CHECK(latest_attempt_generation >= 0),
  latest_action_adapter_id, latest_action_id, latest_action_state,
  open_finding_set_digest NOT NULL,
  repair_required_finding_set_digest NOT NULL,
  prior_target_generation, prior_target_head_evidence_id,
  revision NOT NULL CHECK(revision >= 1), updated_at NOT NULL,
  PRIMARY KEY(run_id,target_generation,slot),
  CHECK((head_generation=0 AND head_evidence_id IS NULL) OR
    (head_generation>=1 AND head_evidence_id IS NOT NULL)),
  CHECK((latest_attempt_generation=0 AND latest_action_adapter_id IS NULL AND
      latest_action_id IS NULL AND latest_action_state IS NULL) OR
    (latest_attempt_generation>=1 AND latest_action_adapter_id IS NOT NULL AND
      latest_action_id IS NOT NULL AND latest_action_state IS NOT NULL)),
  CHECK((prior_target_generation IS NULL)=
    (prior_target_head_evidence_id IS NULL)),
  CHECK(prior_target_generation IS NULL OR
    prior_target_generation=target_generation-1),
  FOREIGN KEY(run_id,target_generation,slot,head_generation,
      head_evidence_id)
    REFERENCES provider_review_evidence(
      run_id,target_generation,slot,new_head_generation,evidence_id),
  FOREIGN KEY(run_id,prior_target_generation,slot,
      prior_target_head_evidence_id)
    REFERENCES provider_review_evidence(
      run_id,target_generation,slot,evidence_id),
  FOREIGN KEY(latest_action_adapter_id,latest_action_id)
    REFERENCES provider_actions(adapter_id,action_id),
  FOREIGN KEY(open_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest),
  FOREIGN KEY(repair_required_finding_set_digest)
    REFERENCES review_finding_sets(finding_set_digest)
)
~~~

Target creation inserts exactly four rows. It carries forward each predecessor slot's complete safe open records and
repair-required sets, but no predecessor evidence becomes current for the new target. Head and attempt generations are
contiguous. Canonical paged set roots are complete/sorted/unique/digest-valid. A head evidence foreign key matches the
same run/target/slot/generation.

provider_review_evidence is immutable. The published relation above is the complete persistence contract for the receipt
`reviewEvidenceRecord`: it names every scalar identity and uses canonical finding-set roots for every stored set. The
actual-route digest is nonnull only through the closed proved endpoint-provider/family/model child bound to the exact
admission observation. Profile/admission or other observed-field inequality retains that proved object as mismatch
evidence; its absence/mismatch blocker and resolution-denial outcome remain inside the immutable evidence JSON and
digest. Currency is never stored in this relation.

`fabric.v1.review-finding-page.read` joins an authenticated session/run to an authorised evidence/completion/receipt
finding-set root, then equality-checks the requested page membership and digest before returning its complete safe
members plus the next ordered page digest. It never accepts a bare globally guessable page digest.
Cross-set/orphan/missing/digest-mismatch rows fail with no partial content. Receipt v2 materialises exactly every
reachable finding page, deduplicated and sorted by page digest, so all set refs are standalone-resolvable and no
unreferenced page is exported.

Dispatch CAS-increments attempt generation and reserves one exact target/slot/head. The safe/UNUSABLE provider terminal
transaction automatically inserts evidence plus reviewEvidenceMutationReceiptV1 and CAS-advances that head before
exposing terminal result. There is no terminal-unrecorded state. UNUSABLE resolves none. The daemon accepts reported
resolutions only for a safe, sufficient-read answer whose target/source/delivery/chair/profile remains current and whose
carried finding is eligible on this successor target. A stale, insufficient-read or same-target repair-required result
accepts none. Every safe P0-P2 finding becomes repair-required automatically. The action- bound terminal transaction is
exempt from live dispatch/annotation currency fences: it always settles and advances its reserved head before
visibility, even after currency drift. Such stale evidence is noncertifying and keeps all safe new findings open for
successor preparation. Provider failure, no-effect/integrity/retired terminal states close only the attempt and create
no review evidence. A later attempt may then reserve the unchanged evidence head. The receipt projector emits provider
terminal failure from the terminal/result/ route joins with unchanged head/open/repair set digests and no
evidence/new-head fields.

Annotation is owned by separate append-only relations:

~~~sql
review_evidence_annotations(
  run_id, evidence_id, annotation_revision, prior_annotation_revision,
  command_id, chair_binding_generation, disposition, note,
  note_digest, annotation_digest, created_at,
  PRIMARY KEY(run_id, evidence_id, annotation_revision),
  UNIQUE(command_id)
)
review_evidence_annotation_heads(
  run_id, evidence_id, current_annotation_revision, revision,
  PRIMARY KEY(run_id, evidence_id),
  FOREIGN KEY(run_id, evidence_id, current_annotation_revision)
    REFERENCES review_evidence_annotations(
      run_id, evidence_id, annotation_revision)
)
~~~

Disposition CHECK is exactly `substantiated|unsubstantiated|duplicate|needs- more-evidence`; note is inert UTF-8 at most
512 bytes. Rows are immutable, revisions are contiguous and the head CAS gives one current projection.
`fabric.v1.review-evidence.annotate` writes only this relation against exact evidence/result/head and active chair
binding. It cannot create evidence or change head/verdict/findings/repair/reviewer-family relation/currency/completion.
Exact replay returns its immutable receipt before live-chair check. Receipt v2 and completion queries never join either
annotation table. The dispatch command's original prepared/dispatched receipt is immutable and never gains terminal
fields on replay. Terminalisation has a separate internal action-pair/target/slot/attempt idempotency journal and stores
the canonical terminal- input digest over terminal kind, private answer/adapter-result digests, authenticated usage and
read-coverage journal. Exact duplicate returns the stored terminal projection. Changed live-callback/lookup input
appends an integrity quarantine, never overwrites result/evidence/head/settlement and makes completion emit
integrity-failure. provider-action.read exposes the terminal result and automatic mutation receipt. Neither receipt
contains currency.

A first or second FINDINGS action therefore always progresses linearly. A repaired target carries each prior finding's
full safe content and origin action/result as mandatory bundle evidence, then permits CLEAN to close it. Target
preparation Phase B rejects any predecessor nonterminal action and cannot commit until every safe/UNUSABLE terminal has
atomically reached its head. No source change can launder a late finding.

An ambiguous/awaiting-human-retire attempt remains nonterminal and owns its target/slot/head/reservation. It blocks
sibling dispatch, target reprepare/ supersession and review/run acceptance or close until proved terminal recovery or
confirmed retirement. This freezes budget and gates review/liveness.

Read/list responses join immutable evidence to a freshly derived review_evidence_currency value. Exact command replay
never performs that join. Operator Evidence row/detail uses the exact operatorReviewEvidenceRowV1 union under operator
project/session/run scope. Its view joins task/action pair, terminal kind/safety/failure code, answer/result,
route/final-prompt, adapter/family/ model, bundle/coverage, severity/open counts, reviewer-family relation, active chair
binding and the one current annotation disposition/revision/digest plus safe detail fields without raw content.

### Completion and deterministic projection

ReviewCompletionReducer first reads the persisted required-slot availability. Any false slot returns
`certifying-review-capability-unavailable` plus exact profile-ordered `unavailableSlots[]`, even when no target exists.
Finding- capacity exhaustion has the next target-wide branch and empty slots. Otherwise it runs target currency and
contiguous active-chair-binding checks, then reads one current target, one resolved four-slot profile and exactly four
slot heads. It never scans for an unsuperseded latest row. For each head it validates the latest action-pair/evidence
chain, target/bundle/route/active-chair-binding/profile joins, reviewer-family relation and complete open-finding set.
Its query columns map one-for-one to reviewCompletionV1: target chair/artifact/
lineage/bundle/root/coverage/risk/mandatory/profile digests and, per slot, head/
attempt/action-pair/evidence/verdict/result/route, resolved adapter/family/model, read coverage, reviewer-family
relation, certifying state, complete open records and ordered blockers.

It emits only the ordered closed blockers in the Agent Fabric contract. open-findings is the sole finding blocker. A proved
no-answer/max-turn terminal result emits provider-terminal-failure; terminal no-effect and human-retired unknown emit
their exact blockers; route-integrity covers a terminal but unverifiable route chain. ambiguous-action is reserved for
unproved provider effect/outcome. Missing head evidence emits the slot code only for a structurally valid head whose
current action should own evidence. Zero/multiple/no trustworthy targets use target-null integrity. A trustworthy target
with broken chair binding, profile/head cardinality, CAS chain or immutable join uses target-present integrity:
immutable target fields remain exact, chair/profile are null and slots empty. Missing profile uses its own arm. With a
valid structure exactly four rows exist; stale-target is top-level only. Top and slot blocker enums are disjoint,
`superseded` exists only in historical currency, and a terminal failure row projects unchanged head/open/repair sets
with evidence null. Generated truth-table tests enumerate every arm/cause and reject duplication.

The operator System/Evidence projection and agent completion read call this same reducer. Mutation receipts do not.
fabric-receipt.json exports only closed safe route, target, bundle/coverage/profile, slot-head, evidence and recovery
digests through exact `reviewCompletion`, `providerRoutes`, `providerReviews` and `routeIntegrityRecoveries` codecs in
the Agent Fabric contract the operator-control contract. It contains no raw answer/error, private diagnostics, bundle bytes, portal transcript, prompt,
secret HMAC, adapter result, usage or annotation. The generated standalone Draft 2020-12 schema embeds literal local
enums for objective-check kind, provider failure/substitution code and every registry-closed operator value it uses. It
has no external resolver/dynamic catalogue; an unknown future value rejects and raw provider-specific detail remains
private behind evidence digest.

### ProviderRouteIntegrityRecoveryService

route_integrity_recoveries is one-to-one with an affected certifying provider action and is the only startup/ambiguity
recovery owner for every certifying action, whether its joins are intact or contradictory:

~~~sql
route_integrity_recoveries(
  run_id NOT NULL, adapter_id NOT NULL, action_id NOT NULL, task_id NOT NULL,
  route_ordinal NOT NULL CHECK(route_ordinal >= 1),
  target_generation NOT NULL CHECK(target_generation >= 1),
  slot NOT NULL CHECK(slot IN
    ('native','other-primary','cursor-grok','agy-gemini')),
  attempt_generation NOT NULL CHECK(attempt_generation >= 1),
  recovery_generation, owner_daemon_generation,
  state, reason, terminal_disposition,
  reservation_digest NOT NULL,
  route_state, route_receipt_digest, recovery_evidence_digest,
  lookup_state, lookup_evidence_digest, settlement_digest,
  created_at, updated_at,
  PRIMARY KEY(adapter_id, action_id),
  FOREIGN KEY(adapter_id, action_id, run_id, task_id, route_ordinal)
    REFERENCES provider_actions(
      adapter_id, action_id, run_id, task_id, route_ordinal),
  FOREIGN KEY(adapter_id, action_id, run_id, target_generation, slot,
      attempt_generation, reservation_digest)
    REFERENCES review_finding_capacity_reservations(
      adapter_id, action_id, run_id, target_generation, slot,
      attempt_generation, reservation_digest)
)
~~~

State is detected, inspecting, terminal-proved-no-effect, terminal-proved-usage, awaiting-human-retire or
terminal-retired-unknown. The row joins the exact daemon-global action pair, run and reservation digest; no second
free-form reservation identifier exists. Its task/ordinal/target/slot/attempt tuple is daemon-derived at insert from the
affected certifying action's immutable action/reservation/head custody and never changes; baseline triggers reject any
tuple, pair, run or digest mutation. The displayed composite foreign keys, not mapper prose, bind both custody owners;
this tuple remains trustworthy when the route row itself is missing or integrity- failed. It supplies the existing
public recovery read and scoped route-list filters without reconstructing route bytes. reason and terminal disposition
use the exact closed receipt-v2 enums. lookup_state is not- attempted, in-flight or completed, with evidence digest
non-null exactly for completed. Nonterminal states have null disposition/settlement; proved-no- effect, proved-usage and
retired-unknown use their exact receipt-v2 disposition arm and a non-null settlement digest. Insert fences further
provider I/O, marks the action noncertifying while unresolved and freezes only that reservation's dimensions. All
certifying route/action rows are excluded from generic startup recovery and prepared-action re-enqueue.

The indexed public read by scoped canonical pair returns the exact Agent Fabric contract providerRouteIntegrityRecoveryProjectionV1,
including target/slot/attempt, recovery generation/state/reason, reservation digest, route/lookup/settlement/ evidence
fields and derived retirement eligibility. Operator Evidence emits its closed recovery-action arm. Receipt recovery rows
are watermark audit only and are never accepted as mutation authority.

`route_state` is exactly present, missing or integrity-failed. Present requires the immutable route-receipt digest and
an exact join to provider_action_routes; missing/integrity-failed require that digest null and a non-null safe recovery-
evidence digest. The service owns that discriminator and evidence atomically; no reader, receipt exporter or Console
projection may infer or reconstruct a route from provider, action, bundle or prompt remnants.

The service runs before generic provider recovery. Prepared with durable zero-dispatch proof returns the full
reservation, writes `settled`, and terminalises no-effect. Every dispatched/accepted/ambiguous state permits at most one
bounded pair-keyed lookup when supported. Exact safe/unusable/failure terminal input enters the ordinary action-bound
terminaliser; complete authenticated usage settles exactly and absent/partial usage charges the remaining spendable
reservation. Authenticated closed no-effect returns full capacity under `settled`. A proved effect with an unverifiable
binding conservatively settles as integrity-terminal. Absent, timeout, malformed, conflict or unavailable lookup enters
awaiting-human-retire and retains the reservation. No branch reconstructs route/bundle/prompt, dispatches, retries or
creates evidence outside the ordinary valid-answer terminaliser.

provider-route-integrity-retire is a closed typed operator-action intent. It binds the exact adapter/action pair,
recovery generation, current state and reservation digest; requires external-effect capability, one matching
consequential gate and independently attested direct-human confirmation; and has no provider port. Confirmed Commit
consumes the full remaining spendable reservation, releases only terminal concurrency capacity, records
terminal-retired-unknown and terminalises the action. Wrong/stale authority, gate, generation, digest or confirmation
changes nothing. The human result is labelled retired-unknown, never no-effect or provider-failed.

Preview/Commit load the live row and require exactly `state=awaiting-human-retire` plus the same pair, recovery
generation and reservation digest. The Console cannot construct this action from completion or receipt data and shows it
only when the live projection says eligible.

Each terminal branch atomically settles the reservation, clears its dimension-freeze contribution, terminalises the
action as noncertifying, persists recovery evidence and exits run recovery state when no other blocker exists. After its
bounded inspection deadline, a nonterminal row must be awaiting-human-retire; every other nonterminal state is an
invariant failure. If store corruption prevents identifying the reservation, startup stops mutations under the existing
store-corruption contract rather than leaving a normal route freeze.

### Verification

Deterministic verification covers:

- exact publication-time principal/bridge/custody/session/adapter/model/route joins and target eligibility for each
  source/publisher kind;
- complete base/head changed-file and required-evidence coverage, all bundle limits/digests/chunk chains,
  create-exclusive collision handling and every before/during/phase-B source or delivery mutation; review-diff.v1 exact
  status/mode/binary/rename/path/order/digest fixtures bind one immutable full- ID range, the dynamic final target
  computes its own values and 64 MiB+1 fails;
- preparation acceptance/read, semantic join/conflict, high-water nonreuse, every durable state edge, worker-lease
  restart and CAS/Phase-B crash point, proving one accepted job becomes at most one complete target; the first poll uses
  only the accepted preparation ID and exact session/run scope;
- target/profile creation, exact four-slot reviewer-family mapping, same-agent binding continuity and target
  supersession after source or every unrebindable chair/family/adapter/contract/model/profile advance; public rebind
  execution/replay, non-adopted/crossed/pointer-head negatives and sequential no-ABA binding chains preserve
  target/evidence/finding identity; review-subject JCS golden/permutation/extra/omission/equality-copy fixtures fail
  every crossed nested bundle/profile field;
- contract-bound Claude/Codex/Cursor/Agy exact server/tool/helper/broker sandbox canaries, peer credentials,
  stopped-child persistence, exact provider-closure derivation/substitution negatives, supervisor-FD-3 isolation and
  stub-FD-4– FD-7 closure, daemon/supervisor/startup/PID-reuse cleanup, empty list probes, denied extra methods/effects
  and no cross-bundle portal read;
- structural Python/TypeScript route-schema parity, post-router admission checks, process-tree kill, daemon-global pair
  single-flight, requested/ resolved adapter equality, cross-run conflict, different-adapter same-ID allowance, changed
  concurrent input and replay without router;
- current-chair certifying dispatch and ordinary non-review authority parity;
- safe CLEAN/FINDINGS, UNUSABLE, proved max-turn/no-answer/provider failure and true ambiguity, including exact or
  conservative settlement for every proved- effect terminal, no-effect release, ambiguity retention, stale in-flight
  evidence, insufficient CLEAN/FINDINGS classification and private error scans;
- first/second FINDINGS, UNUSABLE, concurrent head forks, full open-set carry-forward, repaired-target CLEAN, immutable
  mutation replay and live read currency;
- every reducer top-level/slot blocker union, operator/agent projection and standalone resolver-free receipt-v2 literal
  catalogues/sort/equality/history/ count/JCS hash, including capability-unavailable-before-target; append-only
  annotation vocabulary/current projection and annotation-free completion/ receipt; and
- every certifying-action recovery branch, bounded lookup, conservative consumption, direct-human retirement, liveness
  exit, generic-recovery exclusion and absence of redispatch/reconstruction. Direct-SQL shape tests prove digest-only
  reservation custody and reject any free-form `reservation_id` column or mapper input.

The current catalogue explicitly rejects provider_review_packets, model_routing_receipts, cross_family_reviews,
modelRoutingReceipts, crossFamilyReviews, recordModelRoutingEvidence and recordCrossFamilyReviewEvidence and
fabric.v1.review-evidence.record.
