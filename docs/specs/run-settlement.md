# Run settlement and continuation

Owner: `crates/run`, `crates/records`, `crates/supervisor`
Anchor: `crates/run/src/settlement.rs`, `crates/run/src/holder.rs`

Paths are relative to `tools/front-door/`.

## Settlement

A settlement driver holds an exclusive lease. Journal locks do not span external commands, and
terminal effects have separate exclusion. Import precedes writer branch publication, the terminal
record is committed before custody release, and an import refusal retains private Git for
recovery. Staging, stopped merges and private staged indexes stay in attempt-private Git through
terminal cleanup and remain a worktree-removal fence until reconciled; native index convergence
uses only committed trees and preserves conflicting native staging or operations
([lane Git](run.md#lane-git-and-import)). Each attempt publishes only its private ref; a
sibling-divergent host tip is merged and published when no conflict exists, and otherwise
retains both commits with the conflict and setting named. A lane rewrite replaces its unchanged,
custody-pinned launch tip with a warning and a recovery ref. A later host tip is merged or refused, regardless of remote-tracking refs.
- Why: recovery must resume durable effects without racing another driver or losing work.

Settlement decides whether a Git operation is unfinished before it imports objects or
publishes the branch, on the first pass and on a resume whose recorded phase is already Publish
or Record. It observes every directory the lane's writer can write: the bound worktree's private
Git and the attempt's candidate private registration, falling back to the candidate common
directory for direct Git calls. The registration owns per-worktree operation state and is checked
first, using one marker inventory shared with custody and the object view. An unfinished operation
confined to the lane's own Git warns, names the
commit its `HEAD` denotes whether symbolic or detached and the directory that retains the
operation, then publishes the lane's committed tip and leaves that state intact, never resetting
or cleaning it. Only an operation in the host checkout refuses, before import, with the checkout's
bytes unchanged; a refusal never converges the checkout over an unfinished operation.
- Why: a lane's own stopped merge, cherry-pick or rebase is still its committed work to publish
  and a person's state to resume, while publishing over the host checkout's operation would move
  the branch underneath it.

The **host checkout** is the checkout whose Git directory settlement shares with the owner's
working tree: for a linked lane worktree it is the primary checkout, whose operation markers live
in the common Git directory; when the lane's worktree is itself the primary checkout, that
checkout is the lane's own Git. A bare common directory has no host checkout. Settlement reads the
host checkout but never writes its files or index, so a host operation refuses settlement *only*
when it would leave that checkout mid-operation on a ref settlement moves — the lane branch or the
delivery destination, read from a rebase's `head-name` or the checkout's `HEAD` — other than the
lane's own in-progress operation on its branch. When the lane's worktree is the primary checkout,
a stopped rebase or merge of the lane branch is the lane's own Git: settlement warns, names the
commit and the directory retaining it, and publishes the committed tip with the in-progress state
intact, never resetting or cleaning it. An operation on a moved ref other than the lane's own
branch — the delivery destination — still refuses before import. Every other host operation, such
as an owner mid-rebase on an unrelated branch, is retained intact and reported with a single
`GIT_OPERATION_RETAINED` warning through the warning system, then publication proceeds. A warning
that cannot resolve `HEAD` or a Git directory path names the commit or path `unknown` and never
refuses settlement; a request record that cannot be read or decoded, however, is a refusal and
never drops the delivery destination into a warning. Warnings are recorded once in the writer
terminal record, so a resume does not repeat them and `status` can show them.
- Why: an unrelated owner operation and the lane's own stopped work must not strand the lane, while
  publishing over a host checkout's operation on a moved ref other than the lane's own rebase must
  still refuse before import with the checkout's bytes unchanged. Hiding a moved destination behind
  an unreadable request would turn that refusal into a warning.

The guardian transfers its writer exclusion to recovery, and import and cleanup reuse that guard
for the same scope. Failed steps before import share a durable budget of three failures. A resumed
worker honours `retry_after_ms` before its first pass and paces every later pass. Exhausted
retries keep their cause and stop automatic recovery: status reports settlement held and calls
for explicit reconciliation. A retry deadline overdue by three minutes is flagged stalled in the
status reason, and a stalled custody-path refusal names the remedy (replace a symlink or restore a
path inside the attempt directory).
- Why: a retained writer must not contend with itself or retry an unchanged failure forever.

A lane's temporary scratch directory is never a child of the attempt record directory:
it lives at `<state>/attempt-scratch/<attempt id>/tmp`. GC, writer-Git reads, disk
classification and terminal cleanup resolve that sibling root; no fallback resolves the
old `attempts/<id>/tmp` path. The lane's `provider-state` and its handoff,
the prompt and durable output paths remain inside the attempt directory by design, as do
host-written records. `LaunchPaths::scratch` is a child of the sibling temporary directory,
so an incidental relative write there does not land in the attempt record directory.
- Why: a lane could otherwise write the attempt's own settlement records by walking up
  from its scratch, and a foreign record could not be told from settlement's own
  (#3844, #3947).

GC holds the settlement driver lease for the entire collection of attempt payloads, so
claiming a driver and removing its scratch cannot overlap. For a launched attempt GC
also requires the settlement journal to record completion, including across idle retries;
unknown ownership or an unreadable journal keeps the payload. A never-launched attempt
can be collected once its other custody and effects checks pass. The private Git objects
and review source checkout therefore remain available through import and reconciliation.
- Why: scratch is outside the attempt record directory, so GC can reach it while
  settlement still needs it; losing it turns a correct lane into a failed import or
  a vanished review checkout (#3975).
- Owner: `crates/run/src/retention.rs`, `crates/run/src/settlement.rs`

A result or handoff a lane leaves group- or other-readable in Provenant's private attempt
directory is a recoverable condition, not a settlement refusal. Settlement's read of a
provider-written settlement input — `result.md`, the `handoff.json` snapshot, and the progress
handoff under `provider-state` — tightens that regular, single-link, owned file through its open
descriptor, warns with the path and the old mode, and continues. The class lives on
`AttemptFile` (`PROVIDER_SETTLEMENT_INPUTS`), so a new provider-written input is covered by
declaring it there rather than by naming it at the repair. Host-written private records still
refuse an over-broad mode, and every other read of a repaired name still refuses it, so a store
defect stays visible. A symlink, a foreign owner or a path outside the attempt directory still
refuses: a stalled reason for either names the remedy.
- Why: custody protects the private store and the lane's finished work, so an over-broad mode
  on the lane's own evidence should be repaired rather than strand a completed writer, while a
  repair on an unrelated or host-written record would hide the defect and widen what a reader
  accepts.
- Owner: `crates/run/src/artifacts.rs`, `crates/records/src/attempt_file.rs`, `crates/os/src/fs.rs`

Only the detached worker's acquisition of an exited attempt's writer lock waits, within the
store's lock budget (the SQLite busy timeout), and it wakes as soon as the
lock is released. Contention that outlives the wait is `WriterExclusionBusy`, reported with the
holders (each reserved attempt, its guardian's liveness and, once the guardian has exited, its
settlement owner's). Contention sets a retry deadline but never spends the failure budget and never
seals cleanup or import as failed. While any holder may still release the lock (a live or
unobservable guardian or settlement owner, or an unidentified holder) the wait continues and ages
into the stalled reason. Once it has lasted three minutes with no holder that may release it,
settlement is recorded blocked, naming `provenant reconcile <lane>` and
`provenant reconcile --abandon <lane> --reason <text>`; an explicit reconcile clears the block and
restarts the wait.

Every other path (cleanup inside the worker, queued withdrawal, terminal replay and publication,
loss, probes of a live attempt) tries the lock once. When busy it leaves the cleanup effect
pending without counting a failure and retains the attempt, and the worker retries cleanup of
every terminal writer whose cleanup receipt is not done. A refused Git import ends automatic
cleanup: its pins and scratch are recovery evidence, never cleanup debt.
- Why: another settler or a releasing guardian holding the lock is progress, not failure; a request
  must not stall on another holder, and a lock no live process will release must reach an operator.

Every recorded blocking reason is stamped with its wall time and the recording process ID, and
reconcile and settling reports show it as past evidence (`recorded <N>s ago by process <pid>`),
never as an observation of the current request.
- Why: a reason recorded by an earlier worker must not read as a current failure.

Publication judges the lane head before any host merge. It accepts a tip that descends from the
launch base, the origin default branch, the launch ref, any remote-tracking branch or the host
lane branch when the branch is already in the tip, records the ancestor in the branch reflog and
`publication-ancestor.json`, and warns. A tip descending from none warns by default;
`lanes.history.unrooted_tip = "refuse"` refuses it with a typed cause naming that setting. The
unrooted setting decides only that observation: `warn` and `off` do not override a merge refusal,
so when `lanes.history.rebase = "allow"` and the host branch's history is unrelated to the lane
tip, publication still refuses with `PublicationUnrelated`. When a lane's history diverged from
the host lane branch, publication merges the host branch into the lane tip and publishes the merge
when it is clean. When the host branch still equals the custody-pinned launch tip and the lane
rewrote that tip, publication moves the branch to the lane head with a rewritten-history warning.
This needs no push or remote-tracking ref: neither a stale nor a matching tracking ref proves who
owns a later host commit. A host tip advanced since launch instead takes the merge path. A real
conflict names the files; unrelated histories, a configured `merge.*.driver` and a branch rewound
behind the pinned launch base are typed refusals of their own. Each retains the lane's private
history, and `lanes.history.rebase = "refuse"` retains a diverged or rewritten history without
publishing it. A branch rewound behind its launch base is judged before the unrooted tip, so it refuses without first
warning about a tip that refusal would supersede. When publication displaces a branch tip that is
not an ancestor of the new tip, the same Git transaction retains it at
`refs/provenant/recovery/<displaced-oid>`. Every publication refusal is
final for settlement, which seals it like an exhausted import and releases custody instead of
retrying a branch it cannot change; a lost compare-and-swap depends on a branch that can move and
stays retryable.
- Why: rebasing a lane onto the branch it feeds is legitimate, and a refusal that cannot change
  must not leave a finished lane running.

Settlement warns, without refusing, when a writer's commits add or modify tool-state paths at the
repository root (first segment `.local`, `.config` or `.cache`), naming up to ten. A failure to
inspect the diff warns and continues; only a strict `LANE_TOOL_STATE_COMMITTED` setting turns the
observation into a stop.
- Why: tool state does not belong in a project commit, but a warning must not discard an
  otherwise valid lane's work.

## Outcome before release

The shared Run settlement driver commits the provider outcome, result and handoff once the
provider has exited and its captures are durable. It replays the complete retained provider
stream before classifying and never uses a progress-only prefix after a replay error. An
undecodable frame is an evidence defect naming its capture and line (the typed cause carries only
the line number), and it leaves the outcome unsettled rather than a generic task failure. Proven
custody loss and effective cancellation keep their priority, and descendant cleanup cannot
withhold the decision. A terminal attempt can still be releasing: its reservation and writer
exclusion remain until cleanup proves quiescence and import, publication and release receipts
have settled. Result readers keep the committed outcome and report surviving processes; an early
result never permits a second writer.

The holder records only host-proven facts — process exit, crash, timeout, cancel, Git import and
custody failures, budget, context-handoff and lane-token stops, and route identity — then seals them
as `<attempt>/settlement-evidence.json`. One pure `settlement::classify::settle` reads that sealed
record and publishes the single `OutcomeFact`; no other path writes an outcome, cause, settlement
class, stop or settlement signal for a launched attempt. The store publishes the fact and no longer
classifies. Progress compares the writer's head with the pin sealed at launch: `Moved` only when
both are known and differ, `Unchanged` when both are known and equal, `Unknown` otherwise. Remote
tracking refs, `settlement.base()` and pushes are not evidence, and `Unknown` never yields no-work.
Delivery follows the published fact.
- Why: deciding over evidence read at different moments lets five paths disagree; one decider over
  one sealed record makes re-drives and restarts reproduce the same outcome (#4020, #4024).
- Owner: `crates/run/src/holder.rs`, `crates/records/src/settlement/classify.rs`,
  `crates/run/src/store.rs`

The outcome references an immutable provider-exit snapshot. Cleanup may update its own
process-exit observation but cannot rewrite the provider's stop reason, delivery ordering or
decision, and a stop delivered only to descendants after exit does not cancel completed work.
Deferred Git and delivery effects resume from the same decision after quiescence; a failed
outcome publication is journalled and retried while the guardian retains custody.
- Why: a surviving helper, a late stop or interrupted cleanup must neither hide a finished result
  nor change its evidence.
- Owner: `crates/run/src/holder.rs`, `crates/run/src/store.rs`, `crates/supervisor/src/guardian.rs`

A launch with an unknown acknowledgement is not replayed while custody remains uncertain.
- Why: an unobserved provider may still be writing.

Success requires an observed successful exit, matching provider identity and a validated
non-empty result; capture bytes alone are not completion evidence. A complete read-only answer can
succeed without a handoff. A valid complete handoff before a host token-budget stop also succeeds
when no unfinished-work signal remains, retaining the typed host stop and releasing dependants. A
writer with non-exempt uncommitted work does not deliver it.
- Why: a crash, truncation or wrong identity must not read as success.

A declared stop or a model writer with no accepted declaration makes an otherwise successful
attempt need review, and only the last accepted declaration determines a declared stop.
`handoff_missing` means a model writer accepted no declaration; `handoff_malformed` means
declarations were submitted and none was accepted, with a typed diagnostic naming each dropped
field by serde path (for example `verify[0]`). An adapter writer owes no declaration. Both classes
go to the chair, and graph dependants cannot treat either as success. Custody facts (lost,
unsettled, cancelled) keep priority but the malformed evidence survives in their sealed stop. A
missing declaration keeps the census of uncommitted paths and the pinned head in the terminal
reason; an empty final message is `empty_result` and is appended to that reason, never substituted
for it. One settlement class derives terminal outcome, sealed stop and graph outcome; host stops
preserve valid declarations and never count as negative provider-quality evidence.
- Why: unfinished work must not appear complete or lose its handoff, and all readers need one cause
  for the next decision.

A terminal read-only review holds no Git work, so custody is not uncertain merely because its
departed guardian left a reservation in the attempt directory. Reconcile and `stop` treat that
custody as released with a typed warning naming the slot; they do not re-record the same fence on
every pass. This exception still requires the lock-aware custody release decision: a live custody
lock remains a fence even when reboot recovery cannot acquire it, and must not clear temporary
retention. A writer, an unreadable request, and a retained import fence also fail closed.
After the custody lock is confirmed free, settlement cache cleanup and retention likewise
allow the terminal read-only reservation with the typed warning; their other exclusion and
settlement checks still apply.
- Why: reservations fence commits, staging and imports, which a review does not own, but the custody
  lock must remain an exclusion boundary during concurrent release (#3844).
- Owner: `crates/run/src/reader.rs`, `crates/run/src/controls.rs`

A record that no longer decodes is classified as unreadable, not as proof that custody is held.
Reconciliation acquires writer exclusion and releases the lease only when the reservation
classifies as absent and supervisor evidence proves quiescence. A completed exit with no unresolved
cleanup or retained custody proves same-boot quiescence; an exit still settling does not. Without a
completed exit, a recorded holder must be proved gone (or no holder record may exist). The raw bytes
and their typed source (path, schema, error) are sealed under
`quarantine/` before the reservation is cleared; the database row keeps its original bytes and is
never deleted. Both reconciliation and `release attempt:ID` emit the same host-side typed warning
carrying that source. An unknown holder keeps the fence, so a decode failure alone never releases
custody (#3829).
- Why: format drift must not turn a dead attempt into a permanent fence, but custody remains a real
  boundary, so only positive absence evidence may clear it.
- Owner: `crates/run/src/reader.rs`, `crates/run/src/controls.rs`, `crates/cli/src/run/reconcile.rs`

The lane declares its result with `provenant declare --file <path>|--stdin`, exactly one of which is
required. The command reads `PROVENANT_PARENT_ATTEMPT_ID`, validates the schema v2 JSON through
`Handoff::from_json`, and sends one declaration over the attempt's relay socket. A field error comes
back at once as `declaration field <field>: <error>`, so the lane can fix it and call again. Every line
carries a sequence and a `request_id`: replaying the same id with the same bytes returns the original
receipt, while the same id with different bytes is a conflict. Declarations close at settlement and
the last accepted declaration wins; rejected submissions are journalled but never displace an
accepted one. Prose, fenced blocks and an assistant-written `handoff.json` are never read: none can
settle a lane or change its sealed snapshot. This replaces the prior output-fence parsing and ordering
tolerances: when no declaration is accepted, output prose is not a fallback result (#4020).
- Why: one command and one journal give the host a single auditable declaration channel instead of
  parsing tolerances over model prose; immediate field errors let a lane correct itself in turn.
- Owner: `crates/cli/src/declaration.rs`, `crates/run/src/declaration.rs`,
  `crates/records/src/handoff.rs`

The `<attempt>/handoff.json` snapshot slot is owned by settlement, not the lane. Settlement seals a
`HandoffResult` envelope from the last accepted declaration in `<attempt>/declarations.jsonl`; the
envelope shape and its journal provenance are the proof, so there is no separate tag and no window in
which a snapshot exists without it. Bytes found in the
slot at capture that do not decode as that envelope were written by the provider, including files
left by older launches whose scratch lived under the attempt directory. Settlement keeps those bytes as
`handoff.provider-written.json`, warns that the file was provider-written, clears the slot and seals
the envelope from the journal: a lane with an accepted declaration still settles rather than retrying
forever (#3844).
Only a `HandoffResult` envelope is settlement's snapshot. Before #3078 the slot held a bare
`Handoff` (or `Option<Handoff>`), which is foreign evidence with no compatibility path: an attempt
sealed by that build is reconciled by moving its bare snapshot aside as
`handoff.provider-written.json` and settling from its declaration journal, never by reading it as a
sealed handoff. Bytes that were read but do not decode as an envelope after settlement settle
`handoff_malformed` with the typed diagnostic and keep the raw file, instead of retrying an
unchanging decode. An entry in the slot that cannot be read safely — an oversize file, a symlink, a
hard link, a directory or another unsafe entry type — was left by a provider that is gone at
settlement. Capture renames it aside by name without following or reading it, under
`handoff.provider-written.json` (or the declared `handoff.provider-written.unsafe` when the entry
is not a regular file), warns that it was provider-written, and seals the envelope from the journal. When the preferred evidence name is occupied by a directory, symlink or other non-regular
entry, capture moves the slot entry to the declared unsafe name instead; only when both declared
destinations are unusable does capture fail with the typed error. A transient storage IO failure on
a safe entry is not an unsafe class: it
keeps its typed, retriable cause and does not settle. The launch prompt tells the lane that the
harness writes the attempt's own records, including the sealed `handoff.json`, while its result is
declared with `provenant declare` into `declarations.jsonl`. Provider scratch and
`$TMPDIR` live in the sibling scratch root; settlement still validates ownership of its snapshot slot.
- Why: a lane can discover supervisor records from its scratch and write a bare handoff object that
  fails the sealed-envelope decode; trusting it, or retrying a decode that can never change, strands
  a finished attempt. The single sealed publish lets a settlement resume recognise its own record
  without a second file, and an unchanging unsafe slot is not retried because the provider that
  left it cannot clear it.
- Owner: `crates/run/src/completion.rs`, `crates/run/src/artifacts.rs`, `crates/run/src/launch.rs`

`crates/records/src/handoff.rs` owns the fields, decoding and bounds. A valid JSON object keeps
independently valid and forward fields and drops an invalid declared field whole; the diagnostic
lists dropped paths. Retained fields are evidence for the chair and never turn a malformed
declaration into completion or continuation authority. `blocked_on` is additive
(`schema_version` stays 2): an unknown kind, or one given with another status, is kept, ignored
and warned about, and it sets the sealed cause per [what follows a stop](run.md#what-follows-a-stop).
A malformed declaration never falls back to prose or a progress file, including on interruption;
there is no parse path that can turn trailing text into a declaration. Local persistence is lossless
and disclosure uses a destination projection.

## Identity mismatch keeps writer work

A writer whose sealed head moved from its pinned base keeps that commit when the observed provider
identity contradicts the requested route. Settlement records `review_required` with the typed
`identity_mismatch` cause, the head and a warning naming both identities; neither the commit nor
the evidence is discarded and the lane is not reported failed. A writer with no verified moved
head, and every read-only lane, keeps the fatal mismatch decision.
- Why: a wrong provider identity is a reason for a chair to review real work, not to discard it.
- Owner: `crates/run/src/holder.rs`, `crates/run/src/store.rs`

## Stray lane work

A writer's work is the commits on its bound branch. At settlement, once the terminal check
passes, the host observes stray work only when (a) the primary checkout now sits on a branch that
did not exist at launch, or (b) commits reachable from the primary's HEAD or a new or advanced
local branch are reachable from none of the launch tips, the current remote-tracking tips that
existed at launch, or any other linked worktree's HEAD. The host records the disposition as
`PrimaryCheckout` and the uncommitted census in the sealed evidence; `settle` alone decides the
outcome and signals from those facts. How the lane settles depends on where its own work is:

- The bound branch tip differs from the tip sealed at launch: the lane worked in the right place.
  The primary move is not attributed to it, so settlement warns naming the new primary HEAD and
  the lane still settles `succeeded`. The move is left intact; no `stray_work` signal is raised.
- The bound branch stayed at base: the work may be in the primary checkout, so the attempt settles
  `review_required` with the `stray_work` signal, failed as a graph outcome so dependants stay
  held. The reason names the branch or commits, the primary checkout and the bound worktree.

Nothing else is reported: a primary that merely moved is no finding, and a missing position record
or a comparison that cannot run warns while the lane still settles. Only host Git facts decide
this; handoff prose is never parsed. A lane with uncommitted edits keeps its `uncommitted`
outcome. The reason text states only observed facts and never claims the bound branch gained no
commits.
- Why: settlement must not say `succeeded` for a lane whose work sits on a stray branch in the
  primary checkout while its bound worktree stays at base, nor fail a lane whose bound branch did
  advance for a primary move another process made. Git cannot attribute that move to any actor, so
  the bound-branch tip is the evidence: work there means the lane kept to its scope, and the move
  is surfaced as a warning for the chair. Fetched tips and other worktrees' heads account for
  ordinary moves. The check asks for review or warns; it never refuses a lane.
- Owner: `crates/run/src/writer.rs`, `crates/run/src/holder.rs`

## Declared settlement policy

Settlement behaviour is data, sealed with the evidence. Resolution uses `SETTLEMENT_PRECEDENCE`,
lowest first: `Default` < `Host` (`<state>/config.toml [knobs]`) < `Project`
(`.provenant/config.toml [knobs]`, in any repository) < `Route` (the matched route's
`model_policy.defaults.lanes.settlement.<key>` and `rules.policy.lanes.settlement.<key>`). There are no
host-name or project-name special cases. Each value is a `Sourced<T>` carrying its layer and origin;
the effective `SettlementPolicy` is sealed into `settlement-evidence.json` so a re-drive and an audit
reproduce the decision. An invalid value warns `SETTLEMENT_POLICY_INVALID` naming the key, layer and
origin, keeps the default and never refuses. `config show`, `config schema`, `check` and
`run --explain` report the same value and source, and status names an `AppliedPolicy` (its key,
value and source) when a non-default value changed the outcome or the settlement class. Each key
has its own value type, so a value another key allows is invalid here.

| Key | Allowed | Default | Scope | Effect |
| --- | --- | --- | --- | --- |
| `lanes.settlement.declaration_missing` | `review`, `fail`, `warn`, `off` | `review` | host, project, route | Model writer with no accepted declaration; adapter writers owe none. |
| `lanes.settlement.declaration_malformed` | `review`, `fail`, `warn`, `off` | `review` | host, project, route | Declarations submitted, none accepted. |
| `lanes.settlement.empty_result` | `review`, `fail`, `warn` | `review` | host, project, route | Writer's empty final message; a reader's empty result stays `Failed`. |
| `lanes.settlement.uncommitted_work` | `hold`, `review`, `warn`, `off` | `hold` | host, project, route | Never touches files; `domains.run.review_exempt` still decides which paths count. |
| `lanes.settlement.stray_work` | `review`, `warn`, `off` | `review` | host, project, route | `off` skips the primary-checkout check; `warn` keeps `PRIMARY_MOVED_UNATTRIBUTED`. |
| `lanes.settlement.no_work` | `detect`, `off` | `detect` | host, project, route | `off` keeps `DeclaredStop`/`StoppedEarly` on an unchanged head. |
| `domains.fabric.graph.on_stop.<stop_cause>`, `projects.{project}.on_stop.<stop_cause>` | the `OnStop` spellings | the stop table | host, project | Resolved at graph admission; template and node `on_stop` stay above. A pinned cause warns `ON_STOP_PINNED_IGNORED`. |
| `domains.run.declaration_journal_bytes`, `projects.{project}.declaration_journal_bytes` | 65536–16777216 | 1048576 | host, project | `Full` past the limit; under 10% remaining the receipt warns `DECLARATION_JOURNAL_NEAR_FULL`. |
| `domains.run.declaration_relay_seconds` | 1–300 | 10 | host, project | Lane relay timeout; unreadable config warns and uses the default. |
| `domains.run.declaration_relay_retries` | 0–5 | 1 | host, project | Retries reuse the `request_id`; 0 disables retry. |
| `policy.SETTLEMENT_EVIDENCE_REDRIVE_DIFFERS` | `warn`, `off` | `warn` | host, project | The sealed copy always wins; the rule is not refusable. |
| `policy.DECLARATION_JOURNAL_NEAR_FULL` | `warn`, `off` | `warn` | host, project | Boundary-free and not refusable. |

A signal's action changes only the outcome and the warning: the signal itself is always recorded.
`review` turns `Succeeded` into `ReviewRequired`; `fail` turns it into `Failed` with its configured
source; `warn` leaves the outcome unchanged with a warning; `off` leaves it unchanged without one.
With `warn` or `off` the signal drops out of the class ladder, so the class is `Completed` and
dependants release.

Fixed, with reason: progress is pin-relative and `Unknown` is never no-work — calling committed work
"no work" is the defect this batch closes. The sealed evidence wins on a re-drive. A terminal the
host decides without a lane result (a pre-launch refusal, a queued withdrawal, an owner stop after
exit, a lost attempt) builds its evidence with `SettlementEvidence::host_decided` and still goes
through `settle`; no other path writes a settlement class. A lane appends only
to its own socket's attempt. Sequences and `request_id` idempotency are invariants. Prose is never
read: the command guidance is the protocol text and lane rules sit in `lanes.brief_preamble`.
`declared_stop` is the lane's own statement and its follow-up is `on_stop`.

Four auditable values accompany the decider. `GitEvidence.pushed_head` is the remote-tracking tip of
the bound branch at the cut, sealed in `writer-terminal.json`: the full `output` result view prints
`pushed: <oid>`, and `settle` never reads it.
`GitEvidence.uncommitted_paths` is the bounded, sorted census behind the `uncommitted` note and
appears in `output --json` as `uncommitted_paths`. `DeclarationCounts` on `DeclarationEvidence` counts accepted and
rejected declarations, reaching telemetry as `handoff_declarations_accepted` and
`handoff_declarations_rejected`. `OutcomeFact.declaration_source` is `Declared` for an accepted
declaration and `Host` for a host-synthesised stop handoff, reaching telemetry and `output` as
`declaration_source`.
- Why: per-route reliability and provenance need a measure without restoring a prose parser.

## Guardian loss proof

`Recovery::after_guardian_loss` is a separate same-boot recovery capability. It requires the
original guardian lock and attempt custody lock to be free, holds both alongside the original
writer lease, and takes a fresh complete process census under them. Recorded roots, descendants,
unresolved-exit identities and their sessions or groups must have no surviving member, and no
live process may carry the attempt's sandbox mark. A tree whose recorded descendants omitted
identities, or a spawned tree the guardian never completed a census of, is never proven gone
unless the sandbox mark was in effect: a daemon that double-forks and calls setsid after the last
census matches no pid, session or group, and only the mark finds it. An unsandboxed lane lost
before that record is durable is refused, and `reconcile --abandon` is the explicit escape. The
host must also prove worktree non-occupancy under exclusion; unknown or occupied is not free (a
read-only attempt has no worktree). A writer checkout removed from disk is censused like any
other root. Nothing that still exists lies under it, so a process in an existing directory is
outside; a process path with removed components is inside when it splits at the same nearest
existing directory, compared by identity, and its removed components start with the root's. A path
that cannot be split, such as one through a directory this user cannot read, is outside when its
nearest readable directory is none of the root's existing ancestors and no symlink lies below
that directory. An unreadable root or a
dangling symlink stays undecidable, and the root's owner is its nearest existing directory's
owner.

Missing, replaced or unreadable lock evidence never proves loss. Identities are durable before any
guardian can spawn and are rechecked after the worktree census and before release. The capability
retains all three locks while the Run owner commits its canonical Lost outcome and warning, and
only then may the owner consume it to release the reservation; if a durable provider exit
already exists, the driver keeps that outcome and carries the locks through its remaining effects.
Tick queues unknown attempts for this recovery as well as sealed exits; an unknown observation
alone never permits settlement. Dropping the capability leaves the durable fence intact, and a
process checkpoint, elapsed time or reused PID supplies no authority. The capability commits no
outcome itself and does not change read-only presence classification.
- Why: same-boot recovery needs positive death and non-occupancy evidence while the worktree fence
  holds until the outcome is committed.

`reconcile --abandon` of custody not observed absent, such as a dead guardian with unresolved
cleanup, first tries the same proof from a fresh census. A proof makes custody quiescent and
abandonment proceeds; otherwise it refuses and names the blocker, including any live or
unidentifiable PID or omitted descendants. A terminal writer attempt whose retained fence is
absent is different: it is still abandonable. Abandon completes the journal, retains the
worktree and its uncommitted work, and warns with the custody slot that was to hold the fence
instead of treating the record as corrupt.

Once the loss commits, an absent checkout whose
registration proves this attempt's lane ownership is pruned by removing only that Git admin
directory; every other registration, a locked or ambiguous one, and one owned by another
attempt are left alone and warned about with the exact `git -C <common> worktree prune`
remedy. Prune never changes the settled outcome.

An unreadable process alone is not an occupant, but only an explicit host policy may free a
root. Before any excusal the census computes placement evidence for every root from the
process's readable cwd and descriptor paths by directory identity, the same `(dev, ino)`
ancestor test used to place a live path: a cwd equal to or beneath a root is inside it, while
`/` and other ancestors are outside. Two causes stay distinct. A process whose own occupancy
path cannot be placed (for example a descriptor beneath a non-directory, such as launchd's) is
one excusable failure, but a positive placement inside any root still blocks every arm. A root
whose own identity or metadata cannot be read makes placement undecidable for that root: it
fails closed for every policy arm including a configured entry, blocks every excusal, and keeps
that root Unknown. Three policies may then excuse: a listed service uid whose real, effective
and saved uids are all system uids foreign to every readable root owner, consulted for every
failure class (descriptors EPERM, unplaceable process path, cwd unreadable) but never for a human
uid or the owner; an owner-uid process with EPERM-unreadable descriptors when it is
platform-signed (`team_id=platform`) AND its executable is under `/usr/libexec/`,
`/System/Library/` or `/usr/sbin/` AND its cwd was read and placed outside every root by
directory identity — all three credentials must be owner uids, so a setuid binary launched by
the owner is not excused, and `/bin`, `/usr/bin`, unreadable cwd and undecidable root identity
never qualify; and a configured `confinement.census_ignore_unreadable` entry
(bundled default `/usr/bin/login`, `team_id = "platform"`,
`started_before = "root"`), matched on canonical executable path, team id and an
optional `started_before` floor (stable kernel start time at least two seconds
before every scoped root's creation time, or, during `reconcile --abandon` on a
removed checkout, before the earlier of `record.queued_at` and the birth time of
the checkout's Git registration directory (`<common>/worktrees/<id>`), which
`git worktree add` creates before it populates the checkout and a reused or repaired checkout keeps, when
`Service::abandon_writer_creation_floor` proves `retry_of.is_none()`,
`writer_predecessor.is_none()` and `WorktreeOwner` for that attempt under
`WorktreeOrigin::Lane`; retries, continuations, unproven trees, a pruned
registration and an unreported birth time fail closed and leave the attempt
blocked. Two residuals are accepted: both sides of the comparison are
wall-clock times, so a backward clock step larger than the interval between the
registration's creation and a process start inside the tree, plus the two-second
margin, could excuse that process; and Git accepts an existing empty directory
as the target, so a process already inside such a directory before
`git worktree add` predates the registration. Lane checkouts are created at
paths Provenant allocates), extends these defaults to every unreadable failure
class. Each excusal raises a typed warning naming the PID and path. Every other unreadable
process remains Unknown/blocking with its PID named. Unreadable root identity fails closed, and
a listed uid the kernel will not reveal (for example `launchd`, pid 1) cannot be evaluated, so
`launchd` is excused only by a configured entry or by a listed uid whose credentials the code
can actually read — not by the built-in uid listing. The production configured matcher reads
live macOS signing state; a unit test instead pins that the policy observer forwards its
non-empty configured entries to the matcher it calls. Credentials, signature, configuration and
paths are typed observer signals; the `process_details` diagnostic string is never parsed to
decide custody.
- Why: host daemons such as `launchd` and `UserEventAgent` cannot occupy a lane worktree, and
  an unreadable EPERM process must not deadlock every blocked settlement, but a plausible image
  alone must not excuse a process that could be the lane's writer.
- Owner: `crates/confinement/src/worktrees/census.rs`,
  `crates/confinement/src/worktrees/census_excusal.rs`

## Continuation

A retry starts a fresh attempt with the original authority, not a provider session replay. A
preparation refusal creates no successor, and uncertain custody never starts another writer. A
finished task creates no automatic follow-up: once an attempt succeeds, queued steering stays
accepted but resolves as not-continued rather than admitting a second task that would refuse at
preparation and hide the successful attempt. A preparation refusal after a finished task is a
typed operation outcome; the graph keeps the successful attempt and its result instead of
reading as failed.
- Why: retries must preserve authority and avoid concurrent writers, and a never-launched
  successor must not replace the work the writer already finished.

A context handoff stops with a typed cause and a fresh graph continuation can receive the
handoff. A dirty budget boundary takes precedence, and handoff is deferred during budget wrap-up.
- Why: a continuation needs the prior work and a reliable reason for stopping.

Provider identity conflicts and incomplete evidence from delimiter-complete frames, capture loss,
oversize, read failure or drain timeout always fence a same-session steer continuation. The
guardian records an incomplete JSON record at actual EOF separately; settlement may ignore only
that final fragment after a confirmed delivered explicit cancellation or steering interrupt.
Natural completion and uncertain delivery leave the attempt unsettled, and a parseable final
record stays subject to the session and model identity checks.
- Why: an expected interrupted tail may end mid-record without excusing loss of evidence about the
  provider's session.

## Retention and disclosure

`copy_state_classified` copies retained linked files by content under `host` access, with a
warning; `isolated` refuses hard links and skips symlinks. Copies stay within the configured
entry, byte and depth budgets and the source filesystem boundary.

Host-local stdout and stderr captures retain raw provider bytes for replay, because filtering JSONL
before retention can destroy framing and hide the final answer. Local results and provider
continuation state are retained without shape-based rewriting. External publication follows the
`redaction` setting ([supervisor](supervisor.md#disclosure-and-safe-git-evidence) owns its modes);
a failure of an enabled projection withholds publication and leaves the local outcome unchanged.
Raw and exported metadata each bind the bytes they describe, and local capture paths are private
evidence, not publication artifacts.
- Why: the disclosure policy must not corrupt local evidence.

Reconciliation never replays an uncertain launch. Cleanup preserves reservation exclusion except
for released terminal read-only attempts as described above, does not follow links, and preserves
lock identity.
- Why: recovery must not disturb live owners or unrelated paths.

Attempt records reject identity mismatches and unsupported store versions without rewriting the
store, and oversize records fail explicitly instead of truncating ([state](state.md#strict-bounded-records)).
- Why: recovery must not accept altered identity or silently discard evidence.

## Review verdict wording

A review result requires exactly one verdict line outside fenced blocks, and the result ends
with it: a reader owes no declaration, so nothing follows the verdict. Every review prompt,
including synthesis and the one automatic verdict correction, says to end with the verdict line. `ResultCapture` owns recognition,
case-insensitively: PASS, FAIL and INCONCLUSIVE are canonical; approve, approved and lgtm mean
PASS; reject, request changes and changes requested mean FAIL. Trailing text after a complete
recognised word is ignored, and `Verdict FAIL: explanation` is recognised. Accepted noncanonical
wording records a warning quoting the line and its normalisation. Unknown words stay invalid,
quoting the line, and multiple declarations stay invalid, synonymous ones included. In turn-aware
settlement a later text-only acknowledgement may keep an earlier handoff only when no verdict
follows it; once a verdict follows a handoff, the whole suffix must be that one verdict. Provider
turn boundaries still decide whether later tool work invalidates an earlier handoff.

Review launch warns, and proceeds, when a brief prescribes a different verdict shape. Review
results report the pinned reviewed subject as their head, independent of the checkout's launch base.
- Why: ordinary review vocabulary must preserve usable evidence while warnings expose brief and
  output drift before it becomes a settlement surprise.
- Owner: `crates/records/src/result_capture.rs`, `crates/fabric-runtime/src/graph/outputs.rs`,
  `src/fabric/interface.rs`, `src/fabric/review_subject.rs`, `src/fabric/unified.rs`
