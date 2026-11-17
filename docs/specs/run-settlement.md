# Run settlement and continuation

Owner: `crates/run`, `crates/records`, `crates/supervisor`
Anchor: `crates/run/src/settlement.rs`, `crates/run/src/holder.rs`

Paths are relative to `tools/front-door/`.

## Settlement

A settlement driver holds an exclusive lease. Journal locks do not span external
commands, and terminal effects have separate exclusion. Import precedes
writer branch publication, the terminal record is committed before custody release, and
import refusal retains private Git for recovery.
- Why: recovery must resume durable effects without racing another driver or losing work.

## Outcome before release

The shared Run settlement driver commits the provider outcome, result and
handoff once the provider has exited and its captures are durable. Descendant
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
- Why: unfinished work must not appear complete or lose its handoff.

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

The immutable handoff artifact and the handoff inside a sealed stop store the
same typed parse outcome: `Ok(null)` for absent, `Ok(handoff)` for valid, or
`Err({dropped, retained})` for malformed (Serde's externally tagged JSON).
There is no reader for the previous snapshot shape. A malformed final handoff
never falls back to stale progress, including on interruption. Only an absent
final declaration on an interrupted attempt can use its granted progress file;
a malformed or unreadable progress file is also explicit evidence. Diagnostics,
retained fields and forward fields are redacted before persistence or disclosure.

One settlement classification derives the terminal outcome, sealed stop and graph outcome from terminal evidence, reason and handoff.
A missing required writer handoff seals `handoff_missing` and goes to the chair; graph dependants cannot treat it as success.
- Why: all readers need the same cause for the next decision; host stops must
  preserve valid handoffs and never count as negative provider-quality evidence.

## Guardian loss proof

`Recovery::after_guardian_loss` offers a separate same-boot recovery capability.
It requires the original guardian lock and attempt custody lock to be free,
holds both alongside the original writer lease, and takes a fresh complete
process census under those locks. Recorded roots, descendants and their
sessions or groups must have no surviving member. The trusted host must also
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

Known credential shapes are filtered from retained output. A result redaction failure
withholds the result; a session redaction failure retains it. Either prevents
delivery. Result-only orphan recovery preserves its sealed bytes and reports
when it cannot establish redaction.
- Why: a failed filter must not silently publish credentials.

Reconciliation never replays an uncertain launch. Cleanup does not delete under
a reservation or follow links, and preserves lock identity.
- Why: recovery must not disturb live owners or unrelated paths.

Attempt records reject identity mismatches and unsupported store versions
without rewriting the store. Oversize records fail explicitly rather than being
truncated. See [state](state.md#strict-bounded-records) for record boundaries.
- Why: recovery must not accept altered identity or silently discard evidence.
