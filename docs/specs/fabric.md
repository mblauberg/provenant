# Fabric

Owner: `crates/fabric-store` (SQLite), `crates/fabric-runtime` (identity,
coordination, lane relay, peers, graphs, Git transport)
Anchor: `crates/fabric-store/src/db.rs`, `crates/fabric-runtime/src/graph.rs`,
`crates/fabric-runtime/src/relay/mod.rs`

Fabric is the daemonless coordination layer for one user's agents and hosts. It
grants no authority to install, land or run a provider, and keeps no second run
status ([run](run.md)). ADRs
[0020](../adr/0020-retire-the-daemon-fabric.md),
[0021](../adr/0021-configured-workspace-dispatch-boundaries.md),
[0026](../adr/0026-federate-fabric-across-own-hosts-over-ssh.md) and
[0039](../adr/0039-graph-of-templates.md) record the decisions; the
[peer host runbook](../runbooks/fabric-hosts.md) and `provenant help --all` own
use, flags and limits. Paths are relative to `tools/front-door/`.

## Identity and store

Chair custody and session checkpoints derive the actor through the same
harness-aware inbox identity; when `PROVENANT_SESSION_ID` is unset, custody uses
the actor's saved session if one exists.

### Projects never merge by guess

`ProjectId` derives from the host and canonical Git common directory, so linked
worktrees share it; the cross-host key is home-relative and verified at each
peer. No scan or URL heuristic joins projects.
- Why: two checkouts must never share coordination state by accident.

### Lane store isolation

Lanes open the database read-only and no lane profile grants writes to it, its
companions or Fabric locks. A lane reaches the host only through its own
attempt-bound socket, where the host, not the request, fixes the sender; a
caller-supplied sender is rejected. `fabric mail send` uses that relay inside a
lane, and other CLI coordination mutations refuse.
- Why: a lane must not forge records that the host or another agent trusts.
- Owner: `crates/fabric-runtime/src/relay/host.rs`, `crates/fabric-store/src/db.rs`

### Private, versioned, atomic store

Coordination and the host custody ledger use private SQLite databases. Symlinked
or shared components and a different SQLite format major are refused before any
mutation, and config may lower limits but never raise them. A mutation and its
event commit together, and no transaction stays open across waits, spawns, Git or
SSH. A retried operation ID returns its stored receipt; the same ID with a
different digest conflicts. Same-major schema steps follow the
[SQLite store rule](state.md#sqlite-stores).
- Why: a swapped database could forge fences, and agents retry after timeouts.

### Custody and cursors

Fabric claims and graph chair identity share one host-owned custody ledger.
Subject kinds are declared once in `records::custody::SubjectKind`. A record keeps
its holder (chair, or attempt nested under a chair), project, origin, state
(active, landed, retired, released), last activity, related artefacts and
dispatch provenance.

Worktree custody identifies an immutable checkout incarnation: the canonical path
association plus a generation published once in the checkout's private Git
directory. Creation, launch and retirement share it, recreating a removed path
allocates a new generation, retired subjects stay immutable and previews create
no identity. `LandedEvidence` names its predicate (subsumed or landed) and proof
source. The custody-owned `RetirementRecord` pins the endpoint-qualified
reference generation, expected commit, authority and initial proof; replay keeps
that intent and records freshly validated proof on each completed step. Under a
landing-based intent a worktree may complete when its tip is subsumed or landed,
reference deletion needs landed proof, and explicit discard is a distinct
authority that never claims landing and pins the archive ID and exact tip.
Worktree-only retirement leaves the reference and its custody. Every worktree
removal first proves a surviving ref retains its tip or durably records a
preservation ref; detached HEAD metadata alone is not preservation. The pending
retirement index covers every unfinished record and clears only when all steps
complete; one lifecycle replay owner resumes steps with fresh evidence, and a
missing checkout does not seal a receipt while reference deletion is pending.
Changed identity or unavailable evidence leaves retirement retryable. Retention
and evidence owners decide eligibility; state alone never authorises deletion.

Claims never expire, lock or refuse overlapping advisory ownership: acquiring
another holder's item warns, records the previous holder in the same transaction
and proceeds. Gone holders leave unattended items for adoption, unknown native
presence stays unknown, and run writer leases still own worktree custody.
Cursors advance monotonically; an unreachable peer is Unknown.
- Why: unattended work must stay discoverable without making advisory custody
  another admission boundary.
- Owner: `crates/records/src/custody.rs`, `crates/fabric-store/src/claims.rs`,
  `crates/config/src/gc.rs`

### Chair bindings follow graph names

The host chair index reads graph subjects in the custody ledger, keyed by project
and graph name. A graph's first launch seals its dispatch provenance and every
later attempt resolves it. Direct host launch observes the chair when its request
names none, without rewriting the request. An explicit chair change or adoption
warns, records the previous holder and changes only advisory custody. Child
graphs inherit their parent's chair; an unknown parent stays unbound.
- Why: every node in a graph resolves the same dispatching chair without a second
  identity mapping.
- Owner: `crates/fabric-store/src/chairs.rs`, `crates/fabric-runtime/src/chairs.rs`

### Lane graph read scope

A lane reads `status`, `output` and `trace` from its own graph and from any
other graph in the same project whose chair session equals its own. The chair
session is sealed on each graph's custody record at submission; the lane read
path compares those records, with no launch flag or per-graph grant. Steer,
stop and retry remain confined to the calling graph. A graph from another
chair session or another project is `not_found`.
- Why: a repair lane needs the chair's full review output, but an unrelated
  session must stay invisible and no write may cross a graph boundary.
- Owner: `crates/cli/src/fabric/graph_relay.rs`,
  `crates/fabric-runtime/src/coordination/facade.rs`

## Human requests

One service for the CLI owns requests and immutable answers, binding project,
host and actor from identity, never request JSON. An answer commits with its
history, delivery intent and receipt in one transaction, and a competing answer
cannot replace the winner. Claimed work stays unverified until host coverage
verifies it. Only an explicit acknowledgement from the exact target consumes an
answer, and an answer never starts, resumes or approves anything. Remote answers
run on the owning host; an unavailable owner is reported, never mirrored.
- Why: a timeout, a forged label or a competing UI must not produce a second
  authoritative answer or stand in for a workflow gate.
- Owner: `crates/fabric-runtime/src/inbox.rs`, `crates/fabric-store/src/human_inbox.rs`

## CLI and relay surfaces

Relay input is a strict tagged enum with no coercion, validated before side
effects, and the relay carries coordination only. A queued relay caller must still
be present with budget left before any effect, so a timed-out call cannot commit.
The CLI and the relay render one typed category per error. Settlement reactions
(graph advance, chair notice, chair log) run once in effect, and notices never
instruct.
- Why: a lenient parser turns a typo into another request, and a lost lane must
  never commit after its deadline.
- Owner: `crates/fabric-runtime/src/relay/`,
  `crates/fabric-runtime/src/coordination/facade.rs`

## Bounded graphs and route fallback

Policy has three operation scopes. Graph-wide replay validation and expansion use
current host then project policy; node decisions add that node's dispatch overlay;
attempt resume, settlement, publication and recovery resolve current host and
project policy and apply the request's sealed dispatch overrides and project key.
Host-only operations without a project use host policy.

The runtime admits a template, document or planner fragment only after checking
schema capability, structural limits, the fully expanded plan, the checkpoint
envelope and selected outputs. A launch retry without a wall clock leaves the
prior checkpoint. Launch and read waits and writer provisioning have independent
sourced allowances (`domains.fabric.graph.launch_wait_seconds`,
`domains.fabric.graph.writer_provision_seconds`). An occupied concurrency
threshold warns and proceeds unless its declared policy refuses; writer custody
remains a Git boundary.
- Why: a graph that starts must be able to finish within its bounds.
- Owner: `crates/fabric-runtime/src/graph.rs`

### One model spelling per route

A route carries one spelling of its model: the full catalogue id
`ADAPTER/ID`, such as `opencode/opencode/deepseek-v4.1-flash-free`. The host
entry point that holds the effective catalogue decides that spelling once:
submit, run edit/patch (in patch preparation, before a candidate is built) and
tier lowering from configuration. The canonicaliser is pure and idempotent: a
declared id that already names the route's adapter is final and returned
byte-for-byte (warning with `ROUTE_MODEL_NOT_CATALOGUED` when the effective
catalogue does not know it), never prefixed again. The exception is a
namespaced adapter (such as `opencode`) whose model segment has no `/`, e.g.
`opencode/glm`: that is a short `PROVIDER/NAME` form and resolves by catalogue
match, a single unique namespaced candidate, or an adapter-prefixed spelling
with that warning. A bare name or a short `NAMESPACE/NAME` resolves the same
way. Applying it to a stored id again changes nothing. Plan fallbacks and
patch-added or replaced nodes are canonicalised on that same pass; exact-model
tiers and batch items emit the full id at production. Admission reads the stored
ids verbatim and never rewrites them, so launch, retry, advance and relay leave
the spelling alone.
Nothing downstream splits, prefixes or re-derives the stored id: preparation,
successor deduplication and prior-model exclusions parse it directly. This is
what lets a batch route match `run implement --model` and a declared route
equal to a ladder hand-off appear once among successors.
- Why: a model id may itself contain `/`, so re-parsing a stored id silently
  changes which model runs.
- Owner: `crates/routing/src/catalog.rs`,
  `crates/fabric-runtime/src/graph/canonical.rs`,
  `crates/fabric-runtime/src/graph/patch.rs`,
  `crates/cli/src/fabric/graph_relay.rs`, `src/fabric/interface.rs`,
  `crates/fabric-runtime/src/graph/next_step.rs`

### Bundled template declarations

The shared `TemplateLibrary` parses and structurally checks every bundled
template, and the shared `Compiler` applies its declared parameter schema before
admission. The CLI catalogue exposes those declarations.
- Why: adding a bundled template must not need another per-template CLI
  subprocess to prove its schema is accepted.
- Owner: `crates/fabric-runtime/src/graph/template.rs`

`after_settled` orders a node after its predecessors settle in any state without
propagating their failure; they stay visible in the graph outcome. Inputs may
select outputs from either kind of direct dependency; unavailable optional
outputs stay absent and required outputs still gate admission.

### Implementation planning

Implementation templates compile to a writer by default. `--plan` or the template
parameter `plan=true` adds a read-only planner whose result the writer consumes;
`domains.fabric.implement.plan` sets the default, `--explain` reports its source
and an explicit parameter wins. `deliver` and `batch` accept `plan`, and a batch
item may override it.

`batch` expands over `items` into slices, each with its own managed worktree. A
slice's `after` names prerequisite item IDs; unknown IDs and cycles are refused at
compile time naming the IDs. `max_parallel` (default
`domains.fabric.batch.max_parallel`, overridable with `--set max_parallel=N`)
caps concurrency, and queued slices show `waiting` with reason `cap` or `after`. A
dependent slice starts from the current integration head after its
prerequisites integrate, an independent one from the graph's pinned base, and
each integrates into `integration_branch` as it settles. A slice whose worktree
the host has only planned is not yet on any branch, so pre-attempt admission
skips its delivery-target comparison and provisioning verifies the checkout it
creates. A merge conflict pauses only that slice (`blocked`, with conflicting
files) and the rest keep running. `run --explain` runs the same admission checks,
so its preview cannot pass where the launch's admission refuses.
- Why: single writers start without an implicit planning stage, and batches need
  per-slice routing, ordering and bounded concurrency with isolated conflicts.
- Owner: `src/fabric/interface.rs`, `src/graph_interface.rs`,
  `crates/fabric-runtime/src/graph/template.rs`,
  `crates/fabric-runtime/src/graph/transitions.rs`, `crates/run/src/delivery.rs`

### Graph admission and refusal causes

Launch, retry, edit and relay add/edit build the complete candidate in memory.
Requester binding and PATH, extension syntax (`GraphInput::validate_input`),
prospective loop expansion, structural host limits, the checkpoint envelope and
effective input policy must all pass before anything is published, and the
journal lock covers reading the original state through publication. A refused
candidate leaves the checkpoint and execution binding unchanged. The pure
`admit_candidate` gate owns admission; entry points keep no separate resume
validators.

A structural limit at first admission refuses without publishing. A running graph
that later exceeds one (during expansion or after a host limit is lowered) saves
a `StructuralLimit` pause and resumes once the limit is raised; a cancelled or
settled graph with no pending recovery saves none. A verdict correction that
cannot fit keeps the pause on its failed lens until retried. Only a typed
structural overrun gets `StructuralLimit`; malformed fragments stay
`InvalidPlanExpansion`. Persisted topology is checked before prospective
topology, so a corrupt checkpoint keeps its node and `bad_checkpoint` diagnosis.
Extensions have no separate cap; the retained extension list counts toward the
checkpoint envelope. A plan rejection carries its node and validator reason
through the candidate into `dispatch_refused`, so the repair text that says to
fix the named node and reason can be followed.

Refusals may name node IDs, loop keys, field paths that passed the identifier
grammar, and host-owned rule identifiers and schema positions, and never echo
free-form caller text such as prompt or brief bodies. `SchemaPath` keeps schema
positions only. Graph status keeps route fields verbatim. Over-length values or
control characters warn without dropping the values. Limit causes name the host
limit, never the supplied value; CLI stderr, diagnostic JSON and relay details
project the same cause, and warnings return in command and relay receipts,
detached launches included. Under `secrets`, an operator retry brief holding an
exact known credential value warns and proceeds (never opening credential
stores); under `off` the check is skipped. Automatic writer restarts reuse
accepted input.
- Why: a refusal must neither publish rejected input nor copy it into diagnostics.
- Owner: `crates/fabric-runtime/src/graph/candidate.rs`,
  `crates/fabric-runtime/src/graph/refusal.rs`, `src/fabric/interface.rs`,
  `src/fabric/graph_execution.rs`, `crates/cli/src/fabric/graph_relay.rs`

### Composite settlement

A composite's state, outputs and error summary derive from its members while its
own `when` condition holds; a composite skipped by its own condition keeps that
state whatever its members. An explicit `GraphPatch` skip of a composite stays
authoritative until a later replacement or retry supersedes it, and a patch that
replaces or skips a member recomputes the affected active composite boundaries in
the same checkpoint.
- Why: recovery must validate a checkpoint against the topology it records.
- Owner: `crates/fabric-runtime/src/graph/composites.rs`,
  `crates/fabric-runtime/src/graph/patch.rs`

Review panels settle with partial coverage by default. The panel keeps successful
evidence, marks its verdict inconclusive unless blocking findings require
changes, and names unavailable lenses in its result and coverage warning. A
shortfall warns; a project floor declaring `hard = true` refuses when required
lenses or the reviewer minimum are missing.
- Why: an unavailable reviewer must not discard useful evidence, while a project
  may require coverage for its own boundaries.
- Owner: `crates/routing/src/review.rs`, `crates/cli/src/fabric/graph_relay.rs`,
  `crates/fabric-runtime/src/graph/template.rs`

### Session-ready steering

`provenant steer` accepts bounded, operation-idempotent instructions for the exact
running graph attempt. Each has its own durable text and receipt, and settlement
combines them in arrival order into one continuation. The route digest binding a
steer covers session identity (provider, model, effort, account, host-native
lane), not route-selection history, so a same-identity continuation always
resumes.

Delivery is a session-preserving restart, not live injection. The host's shared
adapter stream decoder records the first validated session identity against the
admitted route and spawned process, and the interrupt is requested through the
attempt's custody owner only after that record is durable; until then the text
stays queued while the provider continues. After settlement the graph resumes the
same provider, model, account and session with the saved text as the next user
turn, keeping writer custody. Before interrupting, the owner prints one line
naming `domains.fabric.steer.fallback` and the layer that set it. When the
session is unavailable the default `fresh_with_handoff` starts a fresh session
with the accepted brief, previous bounded handoff and queued steer text;
`refuse` keeps the steer without a successor. A route, account, session or
process identity mismatch is fatal and never takes the fresh fallback. Adapters
advertise `continuation` only when their run path can retain and resume the
session; other routes are `unsupported` and start fresh. Concurrent restarts are
spaced by `domains.fabric.steer.restart_spacing_ms`.

A writer's continuation reuses the retained worktree, so admission waits for
settlement to release the steered attempt's custody: steering stays queued and
the reaction is retried, never dropped. Stopping a terminal steered attempt
resolves queued steering as not continued (`terminal_stop`). When placement or
the run owner is temporarily unavailable, the graph journal persists a bounded
steer intent for the work key and warns; tick replays the original inbox
operation under its original requester, a committed inbox receipt precedes queue
removal, and identity or lineage conflicts never take this path.

For native launches the host records one session root for the final prepared
candidate, bound to the selected provider, model and account. Settlement and the
next launch check that selector against the sealed applied route; no mutable
account-registry lookup or home scan can redirect identity. The provider receives
the exact saved session ID, its stream decoder must confirm identity and route,
and missing, expired, unreadable or contradictory state stays not-continued.
Native account state is never scanned or copied into the successor, and
settlement scratch cleanup preserves the predecessor's session state until the
continuation has launched. A refused continuation keeps closed facts (authority
rule, field, failure kind) into its settled reason; provider output and identity
values never supply that explanation.

The guardian preserves the typed `steered` stop through exit publication, and run
admits the continuation only when the interrupt was delivered before exit. A live
identity conflict (and incomplete identity evidence from oversized, undecodable or
unreadable output) is sealed separately from the bounded stdout tail, and the
guardian routes a conflict through its protocol-rejection stop. A conflict is
fatal at every setting; for incomplete evidence settlement warns with the policy
source by default, and `policy.PROVIDER_IDENTITY_EVIDENCE_INCOMPLETE` can silence
the warning or refuse settlement and continuation. An undecodable frame reports a
line-numbered `MalformedFrame` defect and leaves the result unsettled. An
incomplete final JSON fragment is tolerated only after a delivered
`ExplicitCancellation` or `Steered` stop, and a parseable final record still
passes the shared adapter checks.

The receipt says `queued` only once the text is durable and claims no interrupt
or continuation. Status exposes `queued`, `resumed` or `not_continued`, the
operation and message IDs and the successor when known; the text itself stays in
the private ledger and is never echoed. `resumed` needs the child's observed
session to match the predecessor's and its sealed route to keep provider, model
and account; a fresh fallback records `fresh_session` on a conflict-free child
session and never claims to have resumed. Missing optional model telemetry does
not negate the acknowledgement, but a known contradiction fences continuation.
Busy, skipped, empty and uncertain launch results stay `queued`; a terminal
refusal, verified session mismatch or incomplete terminal proof under strict
policy becomes `not_continued` with a closed safe cause, durable in run. Replaying
an operation with the same text returns the same intent; reusing its ID with
different text conflicts. The host verifies launch evidence and ownership before
reading steering input and stamps graph, node and attempt identity from the work
key. If natural completion races readiness, settlement adopts the durable intent
when terminal session evidence is valid; a different typed stop wins.

Owner cancellation and continuation admission serialise on the parent attempt's
publication fence. A cancellation accepted before a child exists leaves a durable
explicit-stop intent; once the child exists, graph cancellation follows the
validated continuation chain to its current leaf. The intent never asserts signal
delivery. Steer and owner stops use supervisor's scoped `stop-intent.json`, and
`cancel.json` accepts explicit cancellation only. Settlement may preserve
committed work under an exact delivered steer or a current explicit cancellation;
a later cancellation vetoes continuation without rewriting the earlier stop
evidence, and a replacement intent, foreign requester or malformed record vetoes
the in-flight preservation token. Only the current live continuation leaf may
receive control, authenticated peer requests must match origin and receiver
identities, and missing, conflicting, terminal or unverifiable ownership is
refused.
- Why: stop meaning must not vary between admission, import, publication and
  continuation, and a lane must not direct another attempt.
- Owner: `crates/run/src/holder.rs`, `crates/run/src/settlement.rs`,
  `src/fabric/steer.rs`

### Graph cancellation intent

`stop` first writes a bounded request under a short control lock separate from
the graph journal lock, bound to the graph plan and instance and naming the graph
or one node subtree. A launch owner checks requests at graph and attempt binding
boundaries and saves the cancellation fence before acknowledging and removing the
request. A stop that finds the journal busy returns `cancellation_requested` with
its request ID and pending acknowledgement; re-reading status enqueues nothing.
The owner keeps the accepted attempt receipt before applying cancellation, and a
resume cannot clear a newly acknowledged stop in the same advance.
- Why: a launch may hold the journal lock while preparing a writer, but stop must
  stay durable and available during that work.

### Sources and read roots

Read roots grant reading only, limited to the project, its linked worktrees and
configured roots. A pinned `source` fixes a node's subject before its first
attempt and is materialised as an attempt-owned tree, so later edits cannot change
what a reviewer sees; a missing subject fails closed. A `head` source pins the
newest commit of a writer's continuation chain, and the runtime never resets or
creates a worktree to satisfy a binding. Credential-store and project-local agent
configuration overlaps follow
[plans refuse unsafe roots](supervisor.md#plans-refuse-unsafe-roots).
- Why: scouts share read access while review and repair keep one immutable
  subject.

A relay admission stores one schema-versioned typed receipt with its request
digest in the patch record; replay returns it with `replayed=true` whatever the
current worktree-layout configuration, and a missing or malformed receipt is
invalid.

### Writer continuation

A writer that `continues` an upstream `pr` or `push` writer reuses its worktree
and branch, so repairs update the same pull request. The host pushes without
force and refuses a mismatched branch.
- Why: a repair must land on the reviewed PR and never rewrite a moved branch.

### Deterministic gates

Gates decide without a model or attempt and save the decision before dependants
run. CI gates query the exact pinned SHA, and unavailable checks or policy fail
explicitly. The `domains.wait.pr_*` settings (conditional requests, intervals)
change forge-observer request cadence only, never a gate verdict.
- Why: deterministic decisions need durable evidence without a launch.

### Bounded loops

Admission counts every possible round. A body writer integrates by fast-forward
only and never merges. Exhaustion pauses the graph without delivery, and an
extension never adds rounds twice.
- Why: repeated review and repair needs bounded admission and a human decision at
  the bound.

### Node asks and decide nodes

The inbox owns the question and answer; graph checkpoints link to it, and replay
never creates another request. A decide request stores its optional default and
effective timeout with source. Outcomes distinguish owner answers, answers by
declared default or policy and unanswered deadlines; an unanswered outcome
carries no option and follows its declared disposition without releasing
dependants as if one were chosen. Answers need held authority and the exact
target, and cancellation closes owned requests. A timed-out authority gate has no
default and never grants authority through silence.
- Why: the owner and chair need one authoritative answer.

### Land nodes

Graph landing uses the [landing authority policy](cli.md#landing-adds-no-merge-authority).
An inbox answer alone never approves a land.
- Why: a graph cannot confer authority through an inbox answer.

### Typed node outputs

Settled outputs are bounded and passed to dependants as untrusted data. Invalid
verdict evidence fails a consuming gate and full findings stay retrievable.
Oversized findings use content-addressed graph text while the checkpoint keeps a
bounded summary and reference.
- Why: dependants need evidence without copying or trusting transcripts.

### Publication boundary

Local execution, graph inputs and retained state stay raw and lossless under their
custody and integrity controls. The [disclosure policy](supervisor.md#disclosure-and-safe-git-evidence)
governs egress only: graph and peer exports, forge content and fixture exports.

`PeerAction::Output` declares an `OutputAudience`. A graph execution claim is not
authorisation: the receiver binds the authenticated actor, retained launch
receipt, origin work key and requested attempt before allowing graph transport,
and the sender derives the claim from the retained upstream attempt. All
cross-host payloads, authenticated execution submissions and results included,
pass through `CrossHostTransport`, and authentication never exempts disclosure
policy; same-host execution stays raw. With masking enabled, hand-off attachments
are decoded and projected as text or withheld when they cannot be inspected, and
peer output projects the complete verified text before paging with published
lengths and digests. Projection selects declared free-text fields and keeps
envelope keys, tags, identifiers and metadata; a projected stop handoff keeps its
sealed cause, and already projected pages pass through encoding untouched.
Binary output, over-bound projections and event pages that cannot be fully
projected are withheld.

### Checkpoint before launch

Transitions are pure reducers; the scheduler saves the checkpoint under a stable
attempt key before it launches, so replay returns the existing attempt. A
checkpoint missing launch evidence or its actor is refused.
- Why: a restart must never launch a node twice.

### Checkpoint envelope

Admission bounds the worst-case checkpoint before saving the plan. Graph text,
identity fields and object keys keep their exact bytes in local state and
execution context, through captured fragments, retries, template substitutions
and expansions; heuristic content inspection warns with the field and rule
without revealing the match or refusing. Schema, cycle, conflict and
journal-integrity checks stay mandatory. Content-addressed storage externalises
bytes without rewriting them, every strict read and digest check verifies the
stored bytes as stored, and normalising is idempotent and never repeated after
admission. Resume never fills templates or rewrites a sealed submission, and a
resume path or document hash selects the retained graph identity and its sealed
execution as an exact graph ID does.

Save validates the inline checkpoint before externalising text, then verifies
that durable blobs reconstruct the same representation before publication. The
strict decoder verifies both the envelope digest and the base plan digest, so
publication and landing review reject the same corrupt state; the observation
index uses the validated published value and recovery loads and validates the
committed checkpoint independently. `graph::checkpoint_digest` and
`graph::graph_plan_digest` own the identities; hand-built fixtures use
`FileJournal::prepare_fixture_checkpoint`, and production saves never repair a
supplied plan digest. Corrupt-checkpoint reporting and quarantine are in
[state](state.md#graph-checkpoints-cover-persisted-text). Landing skips unusable
review sources, keeps their graph identities in report warnings, and refuses only
when no independent approval remains.
- Why: an accepted graph must be able to save its checkpoint, and no reader may
  see a state another reader rejects.

### Fallback only for unavailability

A node moves to its next route only on a typed pre-launch authentication, quota or
adapter refusal with custody released, or a classified refusal of a launched
read-only attempt within its [retry](retry-owner.md) budget. Launched writers and
uncertain launches never switch.
- Why: a launched writer may already have had effects.

### Graph delivery

Graph delivery opens at most one final PR, only after every active path succeeds
and custody is released; settlement advances only the matching graph.
- Why: a partial graph must never publish, and settlement must not scan unrelated
  graphs.

## Peer hosts

Each host owns its records and liveness; an unreachable peer is unknown, not
dead. An uncertain remote mutation is resolved by the owner's receipt, never
resent. Remote attempts bind the origin identity, transferred Git objects are
verified before use, and a protocol mismatch is refused before decoding;
cross-host text follows the configured disclosure policy. A peer locator
identifies a registered Git common directory, Git's verified worktree inventory
selects the checkout that owns project configuration, and a bare common directory
stays its own host-only layout root.
- Why: a remote guess or resent mutation could free a live lane or apply twice.
