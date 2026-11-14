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

Success requires an observed successful exit, matching provider identity and a
validated non-empty result. Capture bytes alone are not completion evidence. A
complete read-only answer can succeed without a handoff; a writer with
non-exempt uncommitted work does not deliver it.
- Why: a crash, truncation or wrong identity must not read as success.
- Test: `crates/run/src/dispatch_custody_tests.rs::publication_holder_commits_decoded_success_only_with_exit_zero_and_observed_identity`; `crates/run/src/dispatch_settlement_tests.rs::a_read_only_answer_without_handoff_settles_succeeded`

A declared stop or missing writer handoff makes an otherwise successful attempt
require review. Only a valid final handoff determines a declared stop; a
malformed final handoff does not fall back to stale progress. Partial work
remains visible for delivery or review.
- Why: unfinished work must not appear complete or lose its handoff.
- Test: `crates/run/src/dispatch_settlement_tests.rs::a_declared_stop_settles_review_required`

A stop is sealed once from the recorded terminal cause, host outcome and
handoff in that precedence. The graph acts on that sealed fact.
- Why: all readers need the same cause for the next decision.
- Test: `crates/records/src/format2_tests.rs::each_record_round_trips_and_keeps_an_unknown_field`

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
