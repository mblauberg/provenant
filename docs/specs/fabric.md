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
effects, and the relay carries coordination only. A queued relay caller must
still be present with budget left before any effect, so a timed-out call cannot
commit. The CLI and the relay render one typed category per error. Settlement
reactions (graph advance, chair notice, chair log) run once in effect; notices
never instruct.
- Why: a lenient parser turns a typo into another request, and a lost lane must
  never commit after its deadline.
- Owner: `crates/fabric-runtime/src/relay/`, `crates/fabric-runtime/src/coordination/facade.rs`

## Bounded graphs and route fallback

The graph runtime admits a template, document or planner fragment only after
checking schema capability, structural limits, the fully expanded plan, the
checkpoint envelope and selected outputs. A launch retry without a wall clock
leaves the prior checkpoint.
General launch/read waits and writer provisioning have independent, sourced allowances in `domains.fabric.graph.launch_wait_seconds` and `domains.fabric.graph.writer_provision_seconds`; both default to 55 seconds and accept 1..=14400.
An occupied concurrency threshold warns and proceeds unless its declared policy refuses; writer custody remains a Git boundary.
- Why: a graph that starts must be able to finish within its bounds.
- Owner: `crates/fabric-runtime/src/graph.rs`

### Composite settlement

A composite's state, outputs and error summary derive from its members while its own `when` condition holds. A composite skipped by its own condition retains that state regardless of its members. An explicit `GraphPatch` skip of a composite remains authoritative until a later replacement or retry supersedes it; reconciliation still recomputes the other active composite boundaries in the same checkpoint. A patch that replaces or skips a member recomputes the affected active composite boundaries in the same checkpoint.
- Why: recovery must validate a checkpoint against the topology it records.
- Owner: `crates/fabric-runtime/src/graph/composites.rs`, `crates/fabric-runtime/src/graph/patch.rs`

### Session-ready steering

`provenant steer` accepts bounded, operation-idempotent instructions for the
exact running graph attempt. Each accepted operation has its own durable text
and receipt; settlement combines them in arrival order into one continuation.
The host's shared adapter stream decoder records the first validated session
identity against the admitted provider route and spawned process identity. It
requests an interrupt through the attempt's existing custody owner only after
that readiness record is durable. If readiness is not yet available, the text
stays queued while the provider continues. After settlement, the graph resumes
the same provider, model, account and session with the saved instructions as
the next user turn. It must preserve writer custody and must not fall back to a
fresh session when retained provider state is missing or expired.
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
`queued`. `resumed` requires the child's observed session to match the
predecessor session and its sealed applied route to retain the provider, model
and account. Optional model telemetry keeps the configured identity policy:
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

Owner cancellation and same-session graph continuation admission serialize on
the parent attempt's publication fence. A cancellation accepted before a child
exists leaves a durable explicit-stop intent for the gate; if the child was
already admitted, graph cancellation follows the validated continuation chain
and targets its current leaf. The durable intent does not assert signal delivery.

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
project-local agent configuration only warns. Explanation and launch
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
