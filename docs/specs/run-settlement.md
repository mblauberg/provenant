# Run settlement and continuation

Owner: `crates/run`, `crates/records`, `crates/supervisor`
Anchor: `crates/run/src/settlement.rs`, `crates/run/src/holder.rs`

Test paths are relative to `tools/front-door/`.

## Settlement

A settlement driver holds an exclusive lease. Journal locks do not span external
commands, and terminal effects have separate exclusion. Import precedes
publication, the terminal record is committed before custody release, and
import refusal retains private Git for recovery.
- Why: recovery must resume durable effects without racing another driver or losing work.
- Test: `crates/run/src/settlement.rs::tests`

A launch with an unknown acknowledgement is not replayed while custody remains
uncertain.
- Why: an unobserved provider may still be writing.
- Test: none (gap)

Success normally requires an observed successful exit, matching provider identity and a
validated non-empty result. A valid complete handoff before a host token-budget
stop also succeeds when no unfinished-work signal remains; it retains the typed
host stop and its handoff, and releases graph dependants. Capture bytes alone are not completion evidence. A
complete read-only answer can succeed without a handoff; a writer with
non-exempt uncommitted work does not deliver it.
- Why: a crash, truncation or wrong identity must not read as success.
- Test: `crates/run/src/dispatch_custody_tests.rs::publication_holder_accepts_exit_zero_or_completed_host_budget`; `crates/run/src/dispatch_settlement_tests.rs::a_read_only_answer_without_handoff_settles_succeeded`

A declared stop or missing writer handoff makes an otherwise successful attempt
require review. Only a valid final handoff determines a declared stop. A present
but malformed handoff seals `handoff_malformed`, distinct from `handoff_missing`,
and cannot release graph dependants. Its typed diagnostic names each dropped
field using its serde path (for example `verify[0]`) and preserves the decoder
error. Status and output show that diagnostic rather than claiming no handoff.
Custody facts such as lost, unsettled or cancelled retain priority; the malformed
handoff evidence still survives in their sealed stop.
- Why: unfinished work must not appear complete or lose its handoff.
- Test: `crates/records/src/handoff.rs::tests::declared_fields_round_trip_and_malformed_fields_are_retained_as_typed_outcomes`; `crates/run/src/dispatch_settlement_tests.rs::a_declared_stop_settles_review_required`

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
surrounding whitespace, with the same three spellings. Serialization always
writes canonical arrays, lowercase status and concern objects.
- Test: `crates/records/src/handoff.rs::tests::unambiguous_agent_spellings_decode_through_the_types`

The immutable handoff artifact and the handoff inside a sealed stop store the
same typed parse outcome: `Ok(null)` for absent, `Ok(handoff)` for valid, or
`Err({dropped, retained})` for malformed (Serde's externally tagged JSON).
There is no reader for the previous snapshot shape. A malformed final handoff
never falls back to stale progress, including on interruption. Only an absent
final declaration on an interrupted attempt can use its granted progress file;
a malformed or unreadable progress file is also explicit evidence. Diagnostics,
retained fields and forward fields are redacted before persistence or disclosure.
- Test: `crates/run/src/completion.rs::tests::malformed_final_retains_fields_and_never_uses_stale_progress`; `crates/run/src/completion.rs::tests::unsafe_or_oversize_progress_is_a_typed_error_without_following_links`

One settlement classification derives the terminal outcome, sealed stop and graph outcome from terminal evidence, reason and handoff.
A missing required writer handoff seals `handoff_missing` and goes to the chair; graph dependants cannot treat it as success.
- Why: all readers need the same cause for the next decision; host stops must
  preserve valid handoffs and never count as negative provider-quality evidence.
- Test: `crates/records/src/format2_tests.rs::one_terminal_classification_binds_reason_stop_source_and_graph_result`; `crates/records/src/format2_tests.rs::host_stops_preserve_complete_handoffs_and_completed_budgets_succeed`

## Continuation

A retry starts a fresh attempt with the original authority, not a provider
session replay. Preparation refusal creates no successor; uncertain custody
never starts another writer.
- Why: retries must preserve authority and avoid concurrent writers.
- Test: `crates/run/src/dispatch_temporary_tests.rs::fresh_resume_preserves_lineage_and_authority_without_provider_state`

A context handoff stops with a typed cause and a fresh graph continuation can
receive the handoff. A dirty budget boundary takes precedence, and handoff is
deferred during budget wrap-up.
- Why: a continuation needs the prior work and a reliable reason for stopping.
- Test: `crates/fabric-runtime/src/graph/retry_tests.rs::stopped_writer_restarts_fresh_on_another_route_with_its_handoff`; `crates/supervisor/src/guardian/tests.rs::context_handoff_is_due_only_at_a_reported_context_past_a_set_threshold`

## Retention and disclosure

Known credential shapes are filtered from retained output. A result redaction failure
withholds the result; a session redaction failure retains it. Either prevents
delivery. Result-only orphan recovery preserves its sealed bytes and reports
when it cannot establish redaction.
- Why: a failed filter must not silently publish credentials.
- Test: `src/compose_tests.rs::guardian_capture_masks_split_credentials_before_retention`

Reconciliation never replays an uncertain launch. Cleanup does not delete under
a reservation or follow links, and preserves lock identity.
- Why: recovery must not disturb live owners or unrelated paths.
- Test: `crates/run/src/holder.rs::writer_recovery_accepts_only_matching_terminal_absence`

Attempt records reject identity mismatches and unsupported store versions
without rewriting the store. Oversize records fail explicitly rather than being
truncated. See [state](state.md#strict-bounded-records) for record boundaries.
- Why: recovery must not accept altered identity or silently discard evidence.
- Test: `crates/run/src/tests.rs::host_stores_match_their_schema_and_refuse_other_versions_unchanged`
