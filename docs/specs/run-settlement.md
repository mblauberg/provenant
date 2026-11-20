# Run settlement and continuation

Owner: `crates/run`, `crates/records`, `crates/supervisor`
Anchor: `crates/run/src/settlement.rs`, `crates/run/src/holder.rs`

Paths are relative to `tools/front-door/`.

## Settlement

A settlement driver holds an exclusive lease. Journal locks do not span external
commands, and terminal effects have separate exclusion. Import precedes
writer branch publication, the terminal record is committed before custody release, and
import refusal retains private Git for recovery. Private staged indexes also
survive terminal cleanup and remain a worktree-removal fence until reconciled.
Staging and stopped merges stay in attempt-private Git, with the worktree files
left intact. Native index convergence uses only committed trees and preserves
conflicting native staging or operations. Each attempt publishes only its private ref; a sibling-divergent host tip
refuses publication while retaining both commits for recovery.
- Why: recovery must resume durable effects without racing another driver or losing work.

The guardian transfers its existing writer exclusion to recovery; import and
cleanup reuse that guard for the same scope and directory instead of opening a
second description. Failed steps before import share a durable three-failure
budget. A resumed worker honours `retry_after_ms` before its first pass and
paces every subsequent pending pass even if that deadline has expired. Exhausted
retries retain their cause and stop automatic recovery; status reports settlement
blocked and calls for explicit reconciliation. A retry deadline overdue by three
minutes is flagged as stalled in the existing status reason, including failures
before a driver is claimed.
- Why: a retained writer must not contend with itself or spend CPU retrying an
  unchanged failure indefinitely.

The detached settlement worker waits for an exited attempt's writer lock,
when it takes recovery custody, within the store's lock budget (the SQLite busy
timeout, 30 s by default) and wakes as soon as the lock is released. Contention that outlives the wait is `WriterExclusionBusy`,
reported as `waiting for writer lock <path>, held by <holder>`, where the
holder names each attempt reserved in the slot with its guardian's liveness
and, once that guardian has exited, its settlement owner's. The holder list
labels the waiting attempt's own reservation. Contention sets a retry
deadline but never spends the failure budget, never seals cleanup or import
as failed and never exhausts a resumed worker. A holder may still release the
lock while its guardian is live or unobservable, or while another attempt's
settlement owner (its driver lock or a live recorded owner) is live or
unreadable; a holder no reservation identifies is treated the same way. Then
the wait continues and ages into the stalled reason. Once the wait has lasted
three minutes with no holder that may release it, nothing will: settlement is
recorded blocked, and the reason names `provenant
reconcile <lane>` and `provenant reconcile --abandon <lane> --reason <text>`.
An explicit reconcile clears the block and restarts the wait.

Only that acquisition waits. Every other path, including cleanup inside the
worker and request paths (queued withdrawal, terminal replay and publication,
loss), tries the lock once: when it is busy it leaves the cleanup effect
pending without counting a failure and retains the attempt, and the
settlement worker retries the cleanup of every terminal writer, launched or
not, whose cleanup receipt is not done; retention keeps its scratch until then.
A refused Git import ends automatic cleanup instead: its pins and scratch are
recovery evidence, never cleanup debt. Probes of a live attempt also take the lock once.
- Why: another settler or a releasing guardian holding the lock is progress,
  not failure; counting it blocked settlement and left steered nodes running
  with no successor (#3572). A request must not stall on another holder, and a
  lock no live process will release must reach an operator rather than wait
  forever. Recovery in the same process may hold the lock through another file
  description, so a wait anywhere but the worker's own acquisition stalls for
  the whole budget.

Every recorded blocking reason is stamped with the wall time it was recorded
and the process ID of the worker or reconcile that recorded it. Reconcile and
settling reports show it as past evidence, `recorded <N>s ago by process
<pid>: <reason>` (`recorded earlier` for an unstamped legacy reason), never as
an observation of the current request.
- Why: a reason recorded by an earlier worker read as a current failure and
  misled recovery.

Publication accepts a lane tip that descends from the launch base, the origin
default branch, the ref the lane launched against, or any remote-tracking
branch. It records the ancestor in the branch reflog and `publication-ancestor.json`
and warns. A tip that descends from none of them warns by default; strict
`LANE_TIP_UNROOTED` policy refuses it with a typed cause naming the tip and
launch base. When publication displaces a branch tip that is not an ancestor of
the new tip, the same Git ref transaction retains it at
`refs/provenant/recovery/<displaced-oid>`. That refusal is final:
its inputs do not change on retry, so settlement seals it like an exhausted
import and never retries it. Refusals that depend on a moving branch stay retryable.
- Why: rebasing a lane onto the branch it feeds is legitimate, and a refusal that
  cannot change must not leave a finished lane running.

Settlement warns, without refusing, when a writer's commits between the launch
base and the verified tip add or modify tool-state paths at the repository root:
paths whose first segment is `.local`, `.config` or `.cache`. `WriterLaunch`'s
tool-state report reuses the committed diff behind the declared write-scope
report and names up to ten of the paths, so an incidental tool file cannot hide
in a lane's commits. Ordinary dot paths such as `.github/x` and `src/.config`
are not tool state. A failure to inspect the diff warns and continues; only an
operator's strict `LANE_TOOL_STATE_COMMITTED` setting turns the observation into
a configured stop.
- Why: a lane committed its tool's device identity under `.local/state/` and
  nothing flagged it. Tool state does not belong in a project commit, but a
  warning must not discard an otherwise valid lane's work.

## Outcome before release

The shared Run settlement driver commits the provider outcome, result and
handoff once the provider has exited and its captures are durable. Settlement
replays the complete retained provider stream before classifying the task; it
never uses a progress-only prefix after a replay error. An undecodable frame is
an evidence defect naming its capture and line, and leaves the outcome unsettled
rather than reporting a generic task failure. Its typed terminal cause carries
only the host-observed line number, so public status can name the defect without
exposing the provider error text. Proven custody loss and effective cancellation
retain their existing priority; a signal, budget stop or EOF fragment alone
cannot replace a known capture diagnostic. Descendant
cleanup cannot withhold that decision. A terminal attempt can still be
releasing: its reservation and writer exclusion remain until cleanup proves
quiescence and Git import, publication and release receipts have settled.
Result readers retain the committed outcome and report surviving processes;
an early result supplies no permission to start a second writer.

The outcome references an immutable provider-exit snapshot. Cleanup may update
its own process-exit observation but cannot rewrite the provider's stop reason,
delivery ordering or committed decision. A stop delivered only to descendants
after provider exit does not cancel the provider's completed work. Deferred
Git and delivery effects resume from the same committed decision after
quiescence. Failed outcome publication is recorded in the scoped journal and
retried while the guardian retains custody.
- Why: a surviving helper, a late stop or interrupted cleanup must neither hide
  a finished result nor change its evidence.
- Owner: `crates/run/src/holder.rs`, `crates/run/src/store.rs`,
  `crates/supervisor/src/guardian.rs`

A launch with an unknown acknowledgement is not replayed while custody remains
uncertain.
- Why: an unobserved provider may still be writing.

Success normally requires an observed successful exit, matching provider identity and a
validated non-empty result. A valid complete handoff before a host token-budget
stop also succeeds when no unfinished-work signal remains; it retains the typed
host stop and its handoff, and releases graph dependants. Capture bytes alone are not completion evidence. A
complete read-only answer can succeed without a handoff; a writer with
non-exempt uncommitted work does not deliver it.
- Why: a crash, truncation or wrong identity must not read as success.

A declared stop or missing writer handoff makes an otherwise successful attempt
require review. Only a valid final handoff determines a declared stop. A present
but malformed handoff seals `handoff_malformed`, distinct from `handoff_missing`,
and cannot release graph dependants. Its typed diagnostic names each dropped
field using its serde path (for example `verify[0]`) and preserves the decoder
error. Status and output show that diagnostic rather than claiming no handoff.
Custody facts such as lost, unsettled or cancelled retain priority; the malformed
handoff evidence still survives in their sealed stop.
A missing writer handoff keeps the census of uncommitted paths and the pinned
head in the terminal reason and the sealed writer terminal. An empty final
message is carried by the `empty_result` terminal cause and appended to that
reason, never substituted for it, so the chair keeps the path list and the graph
stays continuable.
- Why: unfinished work must not appear complete or lose its handoff, and the
  empty-message diagnostic must not hide the edits the writer left behind.

A final answer's handoff is its closed `handoff` fence, which always wins.
Without one, a `json` fence that ends the answer (only whitespace after it) and
decodes as a handoff, with `schema_version` and `status`, is accepted with a
warning naming the fallback; a `json` fence followed by other content is
ordinary text.
- Why: providers drift to `json` fences; refusing a complete declaration for
  its fence label loses finished work, while the warning keeps the drift visible.
- Owner: `crates/records/src/handoff.rs`, `crates/run/src/completion.rs`

The canonical review result order is a standalone verdict followed by the
final handoff. Every review prompt, including synthesis and the one automatic
verdict correction, states that order and never invites text after the
handoff. As a narrow provider-order tolerance, a valid closed final
handoff may be followed by exactly one recognised standalone verdict, and
nothing else. ResultCapture owns verdict recognition, so the tolerated line is
any form it counts, and it requires exactly one canonical verdict outside
fenced blocks. In turn-aware settlement, a later
text-only acknowledgement may preserve an earlier handoff only when no verdict
follows it. Once a verdict follows a handoff, the entire suffix from that
handoff must be the sole recognised verdict: prose before or after it, another
verdict, or a repeated handoff remains malformed. Provider-turn boundaries
still decide whether later tool work invalidates an earlier handoff.
Settlement records an accepted order tolerance in the terminal reason; the
sealed handoff payload keeps its existing schema.
- Why: tolerate providers that append their verdict after the closed handoff
  without broadening handoff completion authority.

The handoff declaration owns its fields, per-field decoding and validation.
Decoding a valid JSON object retains independently valid fields and forward
fields, and drops an invalid declared field whole, even when only an array item
is wrong. The diagnostic lists the dropped paths and errors. Retained `note`,
`remaining`, `decisions`, `verify` and concerns are evidence for the chair; they
never turn a malformed declaration into completion or continuation authority.
Invalid JSON, oversized input and invalid fence syntax have the root path `$`;
there is no parsed remainder when the JSON itself cannot be decoded. Existing
byte, item-count and non-empty-text bounds still apply.

`SpecConcern` accepts a string as its claim. Its other text fields default to
empty, and its status defaults to `as_written`: raising a concern does not imply
permission to diverge. Object concerns require a non-empty claim; their other
text may be empty. The `remaining`, `decisions` and `verify` fields also accept a
single string as a one-item array. Handoff status accepts casing differences and
surrounding whitespace, with the same three spellings. Serialisation always
writes canonical arrays, lowercase status and concern objects.

A `blocked` handoff may add `blocked_on` with `owner_decision`, `capability`,
`environment` or `dependency` to say what blocked the writer. The field is
additive: `schema_version` stays 2 and it is never required. A text value that
names no kind, or any value given with another status, is kept, ignored and
warned about; it never refuses the handoff. A value that is not text is
malformed, as for every declared field. Casing
and surrounding whitespace do not matter. The sealed stop takes its cause from
it (`blocked_environment`, `blocked_dependency`, otherwise `blocked`); see
[run](run.md#what-follows-a-stop). The status guidance in the writer preamble
and in the routing policy's lane guidance is one text, owned by
`crates/records/src/handoff.rs`.

Adjacent repeated handoff blocks are one declaration: identical copies are
accepted, and differing copies settle on the last, each with a warning. Prose
between blocks, or after the last, still fails as not ending the answer.
- Why: a decoder or model repeating a finished handoff must not fail a lane that
  committed its work.

The immutable handoff artifact and the handoff inside a sealed stop store the
same typed parse outcome: `Ok(null)` for absent, `Ok(handoff)` for valid, or
`Err({dropped, retained})` for malformed (Serde's externally tagged JSON).
There is no reader for the previous snapshot shape. A malformed final handoff
never falls back to stale progress, including on interruption. Only an absent
final declaration on an interrupted attempt can use its granted progress file;
a malformed or unreadable progress file is also explicit evidence. Diagnostics,
retained fields and forward fields remain lossless in local persistence; disclosure
uses a destination projection.

One settlement classification derives the terminal outcome, sealed stop and graph outcome from terminal evidence, reason and handoff.
A missing required writer handoff seals `handoff_missing` and goes to the chair; graph dependants cannot treat it as success.
- Why: all readers need the same cause for the next decision; host stops must
  preserve valid handoffs and never count as negative provider-quality evidence.

## Identity mismatch keeps writer work

A writer whose sealed head moved from its pinned base keeps that commit when
the observed provider identity contradicts the requested route. Settlement
records `review_required` with the typed `identity_mismatch` cause, the head
and a warning naming both the requested and observed identities; neither the
commit nor the mismatch evidence is discarded, and the lane is not reported as
failed. A writer with no verified moved head keeps the fatal mismatch decision,
as does every read-only lane.
- Why: a wrong provider identity is a reason for a chair to review real work,
  not a reason to throw that work away.
- Owner: `crates/run/src/holder.rs`, `crates/run/src/store.rs`

## Stray lane work

A writer's work is the commits on its bound branch. Launch records the primary
checkout's branch and every local and remote-tracking ref tip (`refs/heads/*`,
`refs/remotes/*`). At settlement, once the writer's terminal check passes, the
host flags stray work only when either holds:
- (a) the primary checkout now sits on a branch that did not exist at launch;
- (b) commits are reachable from the primary's HEAD or from a new or advanced
  local branch that are reachable from none of the launch tips, the current tips
  of the remote-tracking refs that existed at launch, or the HEAD of any linked
  worktree other than the primary (which covers the bound branch and other
  lanes' branches). One `git rev-list --stdin` call decides this.

Stray work does not succeed. The attempt settles `review_required` with the
`stray_work` partial signal and settlement reason, whose graph outcome is
failed so dependants stay held, whatever the bound branch holds. The reason names
the stray branch or the commits, the branches they are on, the primary checkout
and the bound worktree.

Nothing else is reported: a primary checkout that merely moved is not a finding.
No recorded position (a bare repository, or a record that could not be written)
or a comparison that cannot run is no finding, and an unreadable comparison
warns while the lane still settles. Only host Git facts decide this; the handoff
is the lane's prose, so branch or head names in it are never parsed. A lane with
uncommitted edits in its bound worktree keeps its `uncommitted` outcome, which
already needs the chair.
- Why: settlement said `succeeded` for a lane whose work sat on a stray branch
  in the primary checkout while its bound worktree stayed at base (#3611). A
  whole-HEAD comparison blamed lanes for a pull the owner ran during the attempt,
  and missed a lane that committed in the primary and switched back. Fetched
  remote-tracking tips and other worktrees' heads account for those ordinary
  moves, while an owner commit or branch made in the primary checkout during an
  attempt is still flagged, because Git cannot tell it from a lane's. The check
  asks for review; it never refuses or stops a lane.
- Owner: `crates/run/src/writer.rs`, `crates/run/src/holder.rs`

## Guardian loss proof

`Recovery::after_guardian_loss` offers a separate same-boot recovery capability.
It requires the original guardian lock and attempt custody lock to be free,
holds both alongside the original writer lease, and takes a fresh complete
process census under those locks. A tree whose recorded descendants omitted
identities is never proven gone, because no census can check their sessions or
groups. Nor is a spawned tree whose guardian never completed a census of it,
unless the attempt's sandbox mark was in effect: a daemon that double-forks and
calls setsid after the guardian's last complete census is recorded nowhere and
matches no pid, session or group, and only the inherited mark finds it. The
guardian attempts a full census on its first loop and uses a 5 s backstop for
subsequent full censuses. Only a complete full census clears the incomplete
flag, and tracking publication persists it separately; failed or incomplete
observations, scheduling and publication can delay a complete record. An
unsandboxed lane lost before that record is durable is refused, and
`reconcile --abandon` is the explicit escape. Recorded roots, descendants,
identities an unresolved exit named, and their sessions or groups must have no
surviving member, and no live process may carry the attempt's sandbox mark. The trusted host must also
prove worktree non-occupancy under exclusion; unknown or occupied is not free.
For a read-only attempt there is no worktree to occupy.

Missing, replaced or unreadable lock evidence never proves loss. The identities
are durable before any guardian can spawn, so a valid handoff with no recorded
guardian identity can still be recovered under all three original locks.
Identities are rechecked after the host's worktree census and before release.
The capability retains all three locks while the Run owner commits its
canonical Lost outcome and warning; only then may the owner consume it to
release the reservation. If a durable provider exit already exists, the
driver retains that outcome and carries the proof locks through its remaining
effects and release instead. Tick queues unknown attempts for this proof-only
recovery as well as sealed exits; an unknown observation alone never permits
settlement. Dropping or
failing to obtain the capability leaves the durable fence intact. A process
checkpoint, elapsed time or reused PID alone supplies no such authority.
- Why: same-boot recovery needs positive death and non-occupancy evidence while
  preserving the worktree fence until the outcome is committed.

The capability itself neither commits a Run outcome nor changes read-only
presence classification. The Run settlement driver owns its consumption and
the separate EOF outcome and quiescent import/release transactions. Manual
`reconcile --abandon` remains the explicit escape when proof is unavailable.
Abandonment of custody that is not observed absent, such as a dead guardian
that left its cleanup unresolved, first tries the same guardian-loss proof
from a fresh census. A proof makes custody quiescent and abandonment proceeds;
otherwise it refuses and names the blocker, including any live or
unidentifiable PID or omitted descendants. The census-free absence
classification is unchanged: an unresolved cleanup without that proof stays
unknown, because an escaped process is excluded only by the proof.

## Continuation

A retry starts a fresh attempt with the original authority, not a provider
session replay. Preparation refusal creates no successor; uncertain custody
never starts another writer.
- Why: retries must preserve authority and avoid concurrent writers.

A context handoff stops with a typed cause and a fresh graph continuation can
receive the handoff. A dirty budget boundary takes precedence, and handoff is
deferred during budget wrap-up.
- Why: a continuation needs the prior work and a reliable reason for stopping.

Provider identity conflicts and incomplete evidence from delimiter-complete
frames, capture loss, oversize, read failure or drain timeout always fence a
same-session steer continuation. The guardian records an incomplete JSON
record at actual EOF separately; settlement may ignore only that final
fragment after a confirmed delivered explicit cancellation or steering
interrupt. Natural completion and uncertain delivery leave the attempt
unsettled, and a parseable final record remains subject to the ordinary session
and model identity checks.
- Why: an expected interrupted tail may end mid-record without excusing loss of
  evidence about the provider's session.

## Retention and disclosure

Host-local stdout and stderr captures retain raw provider bytes for replay.
Filtering JSONL before retention can destroy framing and hide the final answer.
Local results and provider continuation state are retained without shape-based
rewriting. External publication follows the `redaction` setting: `off` passes
text unchanged, while `secrets` and `full` mask credential values and token
shapes. A failure of an enabled projection withholds publication without changing
the local execution outcome. Raw and exported metadata each bind the bytes they describe; local
capture paths are private evidence, not safe publication artifacts. A publication
failure withholds delivery while preserving local sealed bytes.
- Why: the selected disclosure policy must not corrupt local evidence.

Reconciliation never replays an uncertain launch. Cleanup does not delete under
a reservation or follow links, and preserves lock identity.
- Why: recovery must not disturb live owners or unrelated paths.

Attempt records reject identity mismatches and unsupported store versions
without rewriting the store. Oversize records fail explicitly rather than being
truncated. See [state](state.md#strict-bounded-records) for record boundaries.
- Why: recovery must not accept altered identity or silently discard evidence.

## Review verdict wording

Review results require exactly one verdict declaration outside fenced blocks.
The shared streaming capture recognises the declaration and verdict words
case-insensitively. PASS, FAIL and INCONCLUSIVE remain the canonical values;
approve, approved and lgtm mean PASS, while reject, request changes and changes
requested mean FAIL. Trailing text, including a reviewed SHA or explanation,
is ignored after a complete recognised verdict word or phrase. A declaration
such as `Verdict FAIL: explanation` is also recognised. Accepted noncanonical
wording records a warning quoting the original line and naming its canonical
normalisation. Unknown words stay invalid, with the diagnostic quoting the
line. Multiple declarations remain invalid, including synonymous ones.

Review launch warns when a brief prescribes a verdict shape different from the
canonical standalone `VERDICT: PASS|FAIL|INCONCLUSIVE` contract; it proceeds and
keeps that contract in the reviewer guidance. Review results report the pinned
reviewed subject as their head, independently of the checkout's launch base.
- Why: ordinary review vocabulary must preserve usable evidence while visible
  warnings expose brief and output drift before it becomes a settlement surprise.
- Owner: `crates/records/src/result_capture.rs`, `crates/fabric-runtime/src/graph/outputs.rs`, `src/fabric/interface.rs`, `src/fabric/review_subject.rs`, `src/fabric/unified.rs`
