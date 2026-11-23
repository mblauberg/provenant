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
retains both commits with the conflict and setting named.
- Why: recovery must resume durable effects without racing another driver or losing work.

The guardian transfers its writer exclusion to recovery, and import and cleanup reuse that guard
for the same scope. Failed steps before import share a durable budget of three failures. A resumed
worker honours `retry_after_ms` before its first pass and paces every later pass. Exhausted
retries keep their cause and stop automatic recovery: status reports settlement blocked and calls
for explicit reconciliation. A retry deadline overdue by three minutes is flagged stalled in the
status reason, and a stalled custody-path refusal names the remedy (replace a symlink or restore a
path inside the attempt directory).
- Why: a retained writer must not contend with itself or retry an unchanged failure forever.

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
when it is clean. A real conflict names the files; unrelated
histories, a configured `merge.*.driver` and a branch rewound behind the pinned launch base are
typed refusals of their own. Each retains the lane's private history, and
`lanes.history.rebase = "refuse"` retains a diverged history without attempting a merge. A branch
rewound behind its launch base is judged before the unrooted tip, so it refuses without first
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

A declared stop or missing writer handoff makes an otherwise successful attempt need review, and
only a valid final handoff determines a declared stop. A missing required handoff seals
`handoff_missing`; a present but malformed one seals `handoff_malformed`, with a typed diagnostic
naming each dropped field by serde path (for example `verify[0]`). Both go to the chair, and graph
dependants cannot treat either as success. Custody facts (lost, unsettled, cancelled) keep priority
but the malformed evidence survives in their sealed stop. A missing handoff keeps the census of
uncommitted paths and the pinned head in the terminal reason; an empty final message is
`empty_result` and is appended to that reason, never substituted for it. One settlement
classification derives terminal outcome, sealed stop and graph outcome; host stops preserve valid
handoffs and never count as negative provider-quality evidence.
- Why: unfinished work must not appear complete or lose its handoff, and all readers need one cause
  for the next decision.

The handoff is the closed `handoff` fence of the final answer, which always wins. Without one, a
`json` fence that ends the answer and decodes as a handoff is accepted with a warning naming the
fallback. Adjacent repeated blocks are one declaration (identical copies accepted, differing
copies settle on the last), each with a warning; prose between blocks or after the last still
fails. A review's final handoff may be followed by exactly one recognised standalone verdict and
nothing else; an accepted order tolerance is recorded in the terminal reason.
- Why: providers drift to `json` fences and repeat blocks, and refusing a complete declaration
  for its label loses finished work while the warning keeps the drift visible.
- Owner: `crates/records/src/handoff.rs`, `crates/run/src/completion.rs`

`crates/records/src/handoff.rs` owns the fields, decoding and bounds. A valid JSON object keeps
independently valid and forward fields and drops an invalid declared field whole; the diagnostic
lists dropped paths. Retained fields are evidence for the chair and never turn a malformed
declaration into completion or continuation authority. `blocked_on` is additive
(`schema_version` stays 2): an unknown kind, or one given with another status, is kept, ignored
and warned about, and it sets the sealed cause per [what follows a stop](run.md#what-follows-a-stop).
A malformed final handoff never falls back to stale progress, including on interruption; only an
absent declaration on an interrupted attempt may use its granted progress file. Local persistence
is lossless and disclosure uses a destination projection.

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
passes, the host flags stray work only when (a) the primary checkout now sits on a branch that
did not exist at launch, or (b) commits reachable from the primary's HEAD or a new or advanced
local branch are reachable from none of the launch tips, the current remote-tracking tips that
existed at launch, or any other linked worktree's HEAD. The attempt then settles `review_required`
with the `stray_work` signal, failed as a graph outcome so dependants stay held, and the reason
names the branch or commits, the primary checkout and the bound worktree.

Nothing else is reported: a primary that merely moved is no finding, and a missing position record
or a comparison that cannot run warns while the lane still settles. Only host Git facts decide
this; handoff prose is never parsed. A lane with uncommitted edits keeps its `uncommitted`
outcome.
- Why: settlement must not say `succeeded` for a lane whose work sits on a stray branch in the
  primary checkout while its bound worktree stays at base, nor blame a lane for a pull the owner
  ran. Fetched tips and other worktrees' heads account for ordinary moves; an owner commit in the
  primary during an attempt is still flagged, because Git cannot tell it from a lane's. The check
  asks for review and never refuses a lane.
- Owner: `crates/run/src/writer.rs`, `crates/run/src/holder.rs`

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
read-only attempt has no worktree).

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
unidentifiable PID or omitted descendants.

## Continuation

A retry starts a fresh attempt with the original authority, not a provider session replay. A
preparation refusal creates no successor, and uncertain custody never starts another writer.
- Why: retries must preserve authority and avoid concurrent writers.

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

Reconciliation never replays an uncertain launch. Cleanup does not delete under a reservation or
follow links, and preserves lock identity.
- Why: recovery must not disturb live owners or unrelated paths.

Attempt records reject identity mismatches and unsupported store versions without rewriting the
store, and oversize records fail explicitly instead of truncating ([state](state.md#strict-bounded-records)).
- Why: recovery must not accept altered identity or silently discard evidence.

## Review verdict wording

A review result requires exactly one verdict declaration outside fenced blocks, in canonical
order: a standalone verdict, then the final handoff. Every review prompt, including synthesis
and the one automatic verdict correction, states that order. `ResultCapture` owns recognition,
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
