# Run settlement and continuation

Owner: `crates/run`, `crates/records`, `crates/supervisor`
Anchor: `crates/run/src/settlement.rs`, `crates/run/src/holder.rs`

Paths are relative to `tools/front-door/`.

## Settlement

A settlement driver holds an exclusive lease. Journal locks do not span external
commands, and terminal effects have separate exclusion. Import precedes
publication, the terminal record is committed before custody release, and
import refusal retains private Git for recovery.
- Why: recovery must resume durable effects without racing another driver or losing work.

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
final handoff. As a narrow provider-order tolerance, a valid closed final
handoff may be followed by exactly one recognized standalone verdict, and
nothing else. ResultCapture owns verdict recognition and requires exactly one
canonical verdict outside fenced blocks. Prose between the handoff and verdict,
another verdict, or a repeated handoff remains malformed. Provider-turn
boundaries still decide whether later tool work invalidates an earlier
handoff. Settlement records an accepted order tolerance in the terminal
reason; the sealed handoff payload keeps its existing schema.
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

## Continuation

A retry starts a fresh attempt with the original authority, not a provider
session replay. Preparation refusal creates no successor; uncertain custody
never starts another writer.
- Why: retries must preserve authority and avoid concurrent writers.

A context handoff stops with a typed cause and a fresh graph continuation can
receive the handoff. A dirty budget boundary takes precedence, and handoff is
deferred during budget wrap-up.
- Why: a continuation needs the prior work and a reliable reason for stopping.

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
