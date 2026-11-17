# Fabric

Owner: `crates/fabric-store` (SQLite), `crates/fabric-runtime` (identity,
coordination, lane relay, peers, graphs, Git transport)
Anchor: `crates/fabric-store/src/db.rs`, `crates/fabric-runtime/src/graph.rs`,
`crates/fabric-runtime/src/relay/mod.rs`

Fabric is the daemonless coordination layer for one user's agents and hosts.
It grants no authority to install, land or run a provider, and keeps no second
run status ([run](run.md)). ADRs
[0020](../adr/0020-retire-the-daemon-fabric.md),
[0021](../adr/0021-configured-workspace-dispatch-boundaries.md),
[0026](../adr/0026-federate-fabric-across-own-hosts-over-ssh.md) and
[0039](../adr/0039-graph-of-templates.md) record the decisions; the
[peer host runbook](../runbooks/fabric-hosts.md) and `provenant help --all`
own use, flags and limits. Paths are relative to `tools/front-door/`.

## Identity and store

Chair custody and session checkpoints derive the actor through the same
harness-aware inbox identity. When `PROVENANT_SESSION_ID` is unset, custody
uses the saved session for that actor if one exists.

### Projects never merge by guess

`ProjectId` derives from the host and canonical Git common directory, so linked
worktrees share it; the cross-host key is home-relative and verified at each
peer. No scan or URL heuristic joins projects.
- Why: two checkouts must never share coordination state by accident.

### Lane store isolation

Lanes open the database read-only and no lane profile grants writes to it, its
companions or Fabric locks. A lane reaches the host only through its own
attempt-bound socket, where the host, not the request, fixes the sender.
`fabric mail send` uses that relay inside a lane; other CLI coordination
mutations refuse. A caller-supplied sender is rejected at the relay boundary.
- Why: a lane must not forge records that the host or another agent trusts.
- Owner: `crates/fabric-runtime/src/relay/host.rs`, `crates/fabric-store/src/db.rs`

### Private, versioned, atomic store

Coordination and the host custody ledger use private SQLite databases. Symlinked
or shared components and a different SQLite format major are refused before any
mutation, and config may lower limits but never raise them. A mutation and its event commit together, and no transaction
stays open across waits, spawns, Git or SSH. A retried operation ID returns its
stored receipt; the same ID with a different digest conflicts.
Same-major additive schema steps follow the [SQLite store rule](state.md#sqlite-stores).
- Why: a swapped database could forge fences, and agents retry after timeouts.

### Custody and cursors

Fabric claims and graph chair identity use one host-owned custody ledger.
Subject kinds are declared once in `records::custody::SubjectKind`: graph,
attempt, branch, ref, worktree, pr, issue, path and cloud key. Records retain
the holder (chair or attempt nested under a chair), project, origin,
active/landed/retired/released state, last activity, related artefacts and
dispatch provenance. Worktree custody identifies an immutable checkout incarnation:
its subject contains the canonical path association and a generation published
once in that checkout's private Git directory. Creation, launch and retirement
share this identity. Recreating a removed path allocates a new generation;
retired subjects and their history remain immutable. Previews observe existing
identities without creating them.
`LandedEvidence` names its predicate (subsumed or landed) and proof source.
The custody-owned `RetirementRecord` pins the endpoint-qualified reference
generation, expected commit, authority and initial proof in the host store.
New intent observes the current tip through the generation allocator under
removal custody. Replay preserves that identity and initial intent while
recording freshly validated proof on each completed step.
Remote endpoint identity keeps only a digest of its bound push URL. Detached
worktrees identify their real private `HEAD` through a worktree endpoint and the
same generation allocator. Worktree-only retirement records affect only the
worktree custody subjects; they preserve the reference and its custody.
Each completed subject step stores the proof it used: under a landing-based
intent, a worktree may complete when its tip is subsumed or landed, while
reference deletion requires a landed proof. Explicit discard remains a distinct
authority choice and never claims landing. Worktree discard pins the archive
ID and exact tip captured before removal in that same record. `Removed`
projections carry the completed step proof and durable retirement ID. Retention
and evidence owners decide eligibility; the custody-owned lifecycle
alone records intent, resumable outcomes and removal projections. Every worktree
removal first proves that a surviving ref retains its tip or creates and durably
records a preservation ref; detached HEAD metadata alone is not preservation.
The pending retirement index covers every record with an unfinished step and
clears only when all declared steps complete. One lifecycle replay owner resumes
linked worktree and reference steps with fresh evidence; a missing checkout does
not seal a receipt while reference deletion is pending. Changed identity or
unavailable evidence leaves the retirement retryable. State alone never
authorises deletion.

Claims do not expire, lock or refuse overlapping advisory ownership. Acquiring
another holder's item warns, records the previous holder in the same transaction
as the custody change, and proceeds. Gone holders leave unattended items for
adoption. Unknown native presence stays unknown. Run writer leases still own
Git isolation. Cursors advance monotonically; an unreachable peer is Unknown.
Holder identifiers in warnings use their normal hexadecimal display rather than
debug-struct formatting.
- Why: unattended work must remain discoverable without making advisory custody
  another admission boundary.
- Owner: `crates/records/src/custody.rs`, `crates/fabric-store/src/claims.rs`, `crates/config/src/gc.rs`

### Chair bindings follow graph names

The host chair index reads graph subjects in the custody ledger, keyed by
project and graph name. A graph's first launch seals its dispatch provenance;
every later attempt resolves it. Direct host launch observes the chair when
its canonical request names none, without rewriting that request. An explicit
chair change warns and records the previous holder. Child graphs inherit their
parent's chair; an unknown parent stays unbound. Explicit adoption changes
advisory custody and is recorded. Graph names, rather than attempt or operation
identifiers, address this view.
- Why: every node in a graph resolves the same dispatching chair without a
  second identity mapping.
- Owner: `crates/fabric-store/src/chairs.rs`, `crates/fabric-runtime/src/chairs.rs`

## Human requests

One service for the CLI owns requests and immutable answers, binding
project, host and actor from identity, never request JSON. An answer commits
with its history, delivery intent and receipt in one transaction, and a
competing answer cannot replace the winner. Claimed work stays unverified until
host coverage verifies it. Only an explicit acknowledgement from the exact
target consumes an answer, and an answer never starts, resumes or approves
anything. Remote answers run on the owning host; an unavailable owner is
reported, never mirrored.
- Why: a timeout, a forged label or a competing UI must not produce a second
  authoritative answer or stand in for a workflow gate.
- Owner: `crates/fabric-runtime/src/inbox.rs`, `crates/fabric-store/src/human_inbox.rs`

## CLI and relay surfaces

Relay input is a strict tagged enum with no coercion, validated before side
effects, and the relay carries coordination only. The wire field for child lane
addition is `prompt`, matching the compiled graph node key; stray `brief`
spellings on the relay wire are rejected. A queued relay caller must
still be present with budget left before any effect, so a timed-out call cannot
commit. The CLI and the relay render one typed category per error. Settlement
reactions (graph advance, chair notice, chair log) run once in effect; notices
never instruct.
- Why: a lenient parser turns a typo into another request, and a lost lane must
  never commit after its deadline.
- Owner: `crates/fabric-runtime/src/relay/`, `crates/fabric-runtime/src/coordination/facade.rs`

## Bounded graphs and route fallback

Policy has three operation scopes: graph-wide replay validation and expansion use
current host then project policy; node decisions add that node's dispatch
overlay; attempt resume, settlement, publication and recovery use the sealed
task-profile snapshot. Host-only operations without a project use host policy.

The graph runtime admits a template, document or planner fragment only after
checking schema capability, structural limits, the fully expanded plan, the
checkpoint envelope and selected outputs. A launch retry without a wall clock
leaves the prior checkpoint.
General launch/read waits and writer provisioning have independent, sourced allowances in `domains.fabric.graph.launch_wait_seconds` and `domains.fabric.graph.writer_provision_seconds`; both default to 55 seconds and accept 1..=14400.
An occupied concurrency threshold warns and proceeds unless its declared policy refuses; writer custody remains a Git boundary.
- Why: a graph that starts must be able to finish within its bounds.
- Owner: `crates/fabric-runtime/src/graph.rs`

`after_settled` orders a node after its predecessors settle in any state without
propagating their failure. The predecessors remain visible in the graph outcome.
Inputs may select outputs from either kind of direct dependency; unavailable optional outputs remain absent, while required outputs still gate admission.

### Implementation planning

Implementation templates compile to a writer by default. `--plan` or the
boolean template parameter `plan=true` adds a read-only planner whose result
the writer consumes. `domains.fabric.implement.plan` sets the default and
`--explain` reports its source; an explicit parameter takes precedence.
`deliver` and `batch` accept `plan`, and a batch item may override it with
`{ item = "slice", with = { plan = true } }`. A junior writer without a plan
input, write scope or explicit plan parameter emits the advisory
`unbounded_brief_junior_writer` warning. Set
`domains.fabric.implement.warn_unbounded_junior=false` to suppress it.
- Why: single writers start without an implicit planning stage; callers choose
  when planning adds value.
- Owner: `src/fabric/interface.rs`, `src/graph_interface.rs`, `crates/fabric-runtime/src/graph/template.rs`

### Composite settlement

A composite's state, outputs and error summary derive from its members while its own `when` condition holds. A composite skipped by its own condition retains that state regardless of its members. An explicit `GraphPatch` skip of a composite remains authoritative until a later replacement or retry supersedes it; reconciliation still recomputes the other active composite boundaries in the same checkpoint. A patch that replaces or skips a member recomputes the affected active composite boundaries in the same checkpoint.
- Why: recovery must validate a checkpoint against the topology it records.
- Owner: `crates/fabric-runtime/src/graph/composites.rs`, `crates/fabric-runtime/src/graph/patch.rs`

Launched review panels settle with partial coverage by default. The panel retains successful evidence, marks its verdict inconclusive unless blocking findings require changes, and names unavailable lenses in its result and coverage warning. Coverage shortfalls warn by default; a project floor declaring `hard = true` refuses when its required lenses or reviewer minimum are missing.
- Why: an unavailable reviewer must not discard useful evidence; projects may explicitly require coverage for their own boundaries.
- Owner: `crates/routing/src/review.rs`, `crates/cli/src/fabric/graph_relay.rs`, `crates/fabric-runtime/src/graph/template.rs`, `crates/fabric-runtime/src/graph/composites.rs`

### Session-ready steering

`provenant steer` accepts bounded, operation-idempotent instructions for the
exact running graph attempt. Each accepted operation has its own durable text
and receipt; settlement combines them in arrival order into one continuation.
The route digest that binds a steer covers session identity (provider, model,
effort, account and host-native lane), not route-selection history such as
pool draws or account source, so a same-identity continuation always resumes.
The host's shared adapter stream decoder records the first validated session
identity against the admitted provider route and spawned process identity. It
requests an interrupt through the attempt's existing custody owner only after
that readiness record is durable. If readiness is not yet available, the text
stays queued while the provider continues. After settlement, the graph resumes
the same provider, model, account and session with the saved instructions as
the next user turn. It preserves writer custody. When the provider session is
unavailable, steer.fallback defaults to fresh_with_handoff: settlement warns
with the configuration source, then starts a fresh session using the original
accepted brief, the previous bounded handoff and all queued steer text. Set
domains.fabric.steer.fallback to "refuse" to retain the accepted steer without
a successor. A route, account, session or process identity mismatch remains
fatal and never takes the fresh fallback.

When placement or the accepted run owner is temporarily unavailable, the graph
journal persists a bounded steer intent for the exact work key before replying
with a warning. Tick checks that work key, launch actor and operation again,
then replays the original inbox operation under its original requester identity.
A committed inbox receipt precedes removal from the queue; an ambiguous reply
retries the same operation. Identity or lineage conflicts never take this path.
A later safe refusal retains the accepted text in the private graph history
without keeping an active retry. Generic graph status never echoes that text.
For native provider launches, the host records one session root for the final
prepared candidate and binds it to the selected provider, model and account.
Terminal settlement and the next prepared launch check that host-owned selector
against the sealed applied route; no mutable account registry lookup or broad
home scan can redirect identity. Codex uses the selected root to read its
unchanged v1 rollout metadata and validates the exact saved session and model,
including a locally discovered account whose selection is not persisted in the
accounts registry. The provider receives the exact saved session ID and its
existing stream decoder must confirm that identity and route; missing, expired,
unreadable or contradictory state stays not-continued. Native account state is
not scanned or copied into the successor. Sandboxed state continues from
attempt-private storage; providers that explicitly retain state there, such as
OpenCode, continue through that same private-state owner.
Settlement scratch cleanup must preserve the predecessor's session state until
the continuation has launched. A refused continuation retains closed facts
identifying the authority rule, affected field and failure kind through
preparation and into its settled reason. Provider output and identity values
never supply that public explanation.
The guardian preserves the typed `steered` stop through exit publication, and
Run admits that continuation only when the intentional interrupt was delivered
before exit. A stream identity error also leaves a route- and process-bound
conflict fence when the rejected first frame yields no usable session event;
that fence is not session-readiness evidence. The guardian seals this live
conflict separately from the bounded stdout tail, so a failed ledger write or a
discarded head frame cannot erase it. Oversized frames, undecodable
delimiter-complete frames, unreadable output, or drain-truncated live output
also seal incomplete identity evidence; settlement leaves that result
unsettled instead of admitting a same-session continuation. The guardian
immediately routes a live identity conflict through its existing protocol
rejection stop path while preserving the separate conflict evidence for
settlement. It separately records an incomplete JSON record in the final bytes
only when the stream reaches actual EOF. That final fragment is tolerated only
after a delivered `ExplicitCancellation` or `Steered` stop; natural completion
and uncertain signal delivery remain unsettled. A parseable final record still
passes through the shared adapter checks, so a session or model mismatch always
fences continuation, including when that record has no trailing newline.

The receipt says `queued` only after the text is durable. It does not claim a
provider interrupt or resumed continuation; graph/run status owns those later
outcomes. Status exposes `queued`, `resumed` or `not_continued`,
the stable operation and message IDs, and the successor when known. A
launcher’s typed `launched` receipt records admission but leaves the status
`queued`. `resumed` requires the child's observed session to match the predecessor
session and its sealed applied route to retain the provider, model and account.
A fresh fallback instead records `fresh_session` and requires a conflict-free
observed child session on that same authorised route; it never claims to have
resumed the predecessor's provider session. Optional model telemetry keeps the configured identity policy:
missing telemetry alone does not negate the session acknowledgement, while a
known contradiction fences continuation. A later terminal session mismatch or
incomplete session/capture proof changes that
interim status to `not_continued`; Run retains the earlier live acknowledgement
fact with the terminal disposition. Terminal matching evidence finalises
`resumed`. The disposition is durable in Run even if child evidence is later
retained or collected. Busy, skipped, empty and uncertain launch results stay
`queued`; a terminal refusal, verified session mismatch or incomplete terminal
proof becomes `not_continued` with a closed safe cause. The original text
remains in the private durable ledger in every outcome. Status never
echoes the steering text into generic receipts or status. Replaying an
operation with the same text returns the same intent, while reusing its ID
with different text conflicts. The host verifies launch evidence and
ownership before reading steering input. At local graph admission, the host stamps graph, node and attempt identity from the work key; an accepted request replay preserves that stored identity, and caller-supplied flags never establish it. If natural completion races with
readiness or the interrupt request, settlement adopts the durable intent when
terminal session evidence is valid. A different typed stop wins and leaves the
saved input without an automatic continuation; a later owner cancel still wins
after a delivered `Steered` stop without changing that stop's evidence. If the
live session or process identity changes, or the terminal session disagrees
with the bound expectation, status records the safe mismatch cause instead of
starting a fresh session.

Owner cancellation and same-session graph continuation admission serialise on
the parent attempt's publication fence. A cancellation accepted before a child
exists leaves a durable explicit-stop intent for the gate; if the child was
already admitted, graph cancellation follows the validated continuation chain
and targets its current leaf. The durable intent does not assert signal delivery.
Steer and owner stops use Supervisor's scoped `stop-intent.json` protocol;
all readers share its interpretation. `cancel.json` accepts explicit
cancellation only. Admission and ongoing work ignore only a valid steer for
this scope and operation. Settlement may preserve committed work under an exact delivered steer or a
current explicit owner cancellation. A later explicit cancellation preserves
commits while vetoing continuation; it never rewrites the provider's earlier
stop evidence. A replacement intent, foreign requester or malformed record
vetoes the in-flight preservation token. The guardian validates the requester before
acting and never interprets a steer stored as cancellation.
- Why: stop meaning must not vary between admission, import, publication and
  continuation. A request Run cannot positively match supplies no authority.

This is continuation transport for #3318, not live input injection. Adapters
advertise support as `continuation` only when their run path can retain and
resume the provider session; other routes remain `unsupported`. The shared
live decoder uses each adapter's existing stream parser, so readiness does not
need a second provider parser or a new control protocol. Live injection may
later optimise the same contract without changing its receipt or control
identity.
Only the current live continuation leaf may receive control; authenticated
peer requests must also match the origin and receiver identities. Missing,
conflicting, terminal or unverifiable ownership is refused.

- Why: a lane must not direct another attempt.

### Graph cancellation intent

`stop` first writes a bounded request under a short control lock that is separate
from the graph journal lock. The request is bound to the graph plan and instance,
and may name the graph or one node subtree. A launch owner checks requests at
graph and attempt binding boundaries; it saves the cancellation fence before
acknowledging and removing the request. A stop that finds the graph journal busy
returns `cancellation_requested` with its request ID and pending acknowledgement
state. Re-reading status does not enqueue another request. The owner preserves
the accepted attempt receipt before applying cancellation, and a resume cannot
clear a newly acknowledged stop in the same advance.
- Why: a launch may hold the journal lock while preparing a writer, but stop
  must remain durable and available during that work.

### Sources and read roots

Read roots grant reading only, limited to the project, its linked worktrees and
configured roots. A pinned `source` fixes a node's subject before its first
attempt and is materialised as an attempt-owned tree, so later edits cannot
change what a reviewer sees; a missing subject fails closed. A `head` source
pins the newest commit of a writer's continuation chain. The runtime never
resets or creates a worktree to satisfy a binding.
Registered linked worktrees are verified at their real paths wherever they
live, including `.claude/worktrees` and locations outside the project.
Declared credential stores still refuse every overlapping source or grant;
project-local agent configuration is advisory, with overlap warnings off by
default and configurable through `CONFINEMENT_SOURCE_AGENT_CONFIG_NONE`. Explanation and launch
preparation share the same source admission decision.
- Why: scouts share read access while review and repair keep one immutable subject.

A relay admission stores one schema-versioned typed receipt with its request digest in the patch record; replay returns that receipt with `replayed=true`, independent of current worktree-layout configuration. A missing or malformed receipt is invalid.

### Writer continuation

A writer that `continues` an upstream `pr` or `push` writer reuses its worktree
and branch, so repairs update the same pull request. The host pushes without
force and refuses a mismatched branch.
- Why: a repair must land on the reviewed PR and never rewrite a moved branch.

### Deterministic gates

Gates decide without a model or attempt and save the decision before dependants
run. CI gates query the exact pinned SHA; unavailable checks or policy fail
explicitly. The shared forge observer uses conditional validators by default,
controlled by `domains.wait.pr_conditional_requests`. Its cached result age
uses `pr_slow_interval_ms` while checks run and returns to `pr_interval_ms`
within `pr_near_end_window_ms` of the median duration for that check name,
learned from the latest 64 cached commit pages in the repository observer.
These settings change request cadence only; they do not change a gate verdict.
Their sources are shown by `provenant config show`.
- Why: deterministic decisions need durable evidence without a launch.

### Bounded loops

Admission counts every possible round. A body writer integrates by
fast-forward only and never merges. Exhaustion pauses the graph without
delivery, and an extension never adds rounds twice.
- Why: repeated review and repair needs bounded admission and a human decision
  at the bound.

### Node asks and decide nodes

The inbox owns the question and answer; graph checkpoints link to it. Replay
must not create another request. A stored decide request carries its optional default and effective configurable timeout with source.
Decision outcomes distinguish owner answers, answers by declared default or policy, and unanswered deadlines; an unanswered outcome carries no option and follows its declared disposition without releasing dependants as if an option were chosen.
Answers require held authority and the exact target; cancellation closes owned requests. A timed-out authority gate has no default and never grants authority through silence.

- Why: the owner and chair need one authoritative answer.

### Land nodes

Graph landing uses the [landing authority policy](cli.md#landing-adds-no-merge-authority). An inbox answer alone never approves a land.

- Why: a graph cannot confer authority through an inbox answer.

### Typed node outputs

Settled outputs are bounded and passed to dependants as untrusted data. Invalid
verdict evidence fails a consuming gate; full findings remain retrievable.
Oversized findings use content-addressed graph text while the checkpoint retains a bounded summary and reference.

- Why: dependants need evidence without copying or trusting transcripts.

### Checkpoint before launch

Transitions are pure reducers; the scheduler saves the checkpoint under a stable
attempt key before it launches, so replay returns the existing attempt. A
checkpoint missing launch evidence or its actor is refused.
- Why: a restart must never launch a node twice.

### Checkpoint envelope

Admission bounds the worst-case checkpoint before saving the plan.
Graph text is normalised exactly once, at ingress, by schema path.
`prompt_text::Context` enumerates identity positions (finding IDs, files and
propagation markers, repair item IDs, node references, routes, template names
and sources). Identity fields and all object keys are scanned across the whole
new structured capture, including fields omitted by typed projections. A
credential-shaped identity or key refuses that capture as invalid output; it is
never rewritten. Only declared scalar identity fields retain their bytes: a route
or source object does not exempt its extension values. Every other string is
content, including flattened extension maps and template or parameter
documentation. Content descendants cannot re-enter a schema context through
identity-like keys. Live patches, retry briefs and CI observations use this
representation before planning or hashing. A steer normalises only its new
brief and replacement routes, preserving the rest of its stored node.
Captured findings are parsed and normalised before merging stored upstream
evidence; the remaining fresh capture projection (including handoff fields and
repair item extensions) is normalised once before becoming graph outputs.
Raw transient captures remain available to parse new patches at their own
ingress, and never become persisted graph text. Captured fragments are parsed,
normalised and scanned before storage; retries parse stored fragments without
normalising them again. Template substitutions are scanned after interpolation,
and each new admission's compiled expansion is scanned before commit, since
separately safe fragments can assemble a credential. Scanning preserves original
bytes and token boundaries, including beside redaction markers. Only an assignment
value that is exactly a redaction marker is already redacted; a marker prefix
never exempts further value bytes. This never rewrites stored
nodes or prior expansions. Typed handoffs are normalised at capture before any
consumer retains them. Checkpoints persist
the normalised form. Compaction retains parsed, normalised findings as typed
values in content-addressed storage. Resume never fills templates or rewrites a
sealed submission. Journal binding and compilation move admitted text without
another normalisation pass. Every strict read and every digest check verifies the
stored bytes as stored and never normalises again. Normalising is idempotent.
Content-addressed storage only externalises bytes and never rewrites digested
content. The journal strictly validates the stored representation before
publishing it. The strict checkpoint decoder verifies both the envelope digest
and the base plan digest, so publication and landing review reads reject the same
corrupt state. Landing skips unusable review sources and retains their graph
identities in structured report warnings, including when another source
supplies a valid approval. Enumeration and source-read failures are warnings
too; evaluation continues through the other configured sources, and refuses
landing only when no independent approval remains.

- Why: an accepted graph must be able to save its checkpoint.

### Fallback only for unavailability

A node moves to its next route only on a typed pre-launch authentication, quota
or adapter refusal with custody released, or a classified refusal of a launched
read-only attempt within its [retry](retry-owner.md) budget. Launched writers
and uncertain launches never switch.
- Why: a launched writer may already have had effects.

### Graph delivery

Graph delivery opens at most one final PR, only after every active path
succeeds and custody is released; settlement advances only the matching graph.
- Why: a partial graph must never publish, and settlement must not scan unrelated graphs.

## Peer hosts

Each host owns its records and liveness; an unreachable peer is unknown, not
dead. An uncertain remote mutation is resolved by the owner's receipt, never
resent. Remote attempts bind the origin identity, and transferred Git objects
are verified before use. Protocol mismatch is refused before decoding;
credentials do not cross hosts.

A peer locator identifies a registered Git common directory. Git's verified
worktree inventory selects the checkout that owns project configuration; a
bare common directory remains its own host-only layout root.

- Why: a remote guess or resent mutation could free a live lane or apply twice.
