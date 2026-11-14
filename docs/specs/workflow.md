# Workflow

Owner: `crates/workflow` (coverage, receipts, sessions, chair logs, writing),
`crates/fabric-runtime/src/handoff/session` (session handoff)
Anchor: `crates/workflow/src/coverage.rs`, `crates/workflow/src/store/tests.rs`,
`tests/workflow.rs`

Workflow checks review coverage and keeps receipts, session checkpoints and
chair logs. It grants no merge, install or release authority. Paths are
relative to `tools/front-door/`.

## Findings need one disposition each

Every selected upstream finding needs exactly one disposition and reason; an
identity-only disposition inherits the finding's descriptive fields.
- Why: reviewers reconcile findings by identity, and none may be silently dropped.
- Test: `crates/fabric-runtime/src/graph/graph_synthesis_findings_tests.rs::a_synthesis_must_dispose_of_every_selected_upstream_finding_exactly_once`;
  `::identity_only_dispositions_inherit_selected_findings`

## Review coverage

A review counts only for a retained verdict on the exact subject, at least the
highest author tier, from an independent reviewer; authors, decision makers
(the chair included) and repeats never count. `inconclusive` never counts and
never blocks. Cross-family legs count only on observed family evidence, never
attestation; a waived leg needs a blocker with source references. The
`code-review` skill owns lens choice; the checker's counts may sit below it.
- Why: self-review, a stale subject or an attested family proves nothing.
- Test: `crates/workflow/src/coverage.rs::authors_decision_makers_repeats_and_low_tiers_do_not_count`;
  `::a_review_of_another_subject_never_counts`;
  `::attested_reviewer_never_fills_a_cross_family_leg_without_a_blocker`;
  `crates/workflow/src/bind.rs::failed_headless_wrong_head_and_foreign_delivery_attempts_are_refused`;
  `crates/routing/src/review.rs::selected_panels_keep_change_driven_lenses`
- Owner: `crates/workflow/src/coverage.rs`, `crates/workflow/src/bind.rs`

## Receipts

A receipt records the chair's decision on one exact subject; a changed subject
resets it. The edge takes the repository and base from the forge, never the
request, so a delta review cannot shrink the covered paths. A downgrade needs a
recorded reason and never passes a crucial or terminal path floor. A receipt
whose shape this build cannot read approves nothing. Saves replace the whole
receipt at an expected revision: one winner, never a torn file.
- Why: acceptance must track the exact subject reviewed, and concurrent chairs
  must not lose decisions.
- Test: `crates/workflow/src/store/tests.rs::changing_subject_resets_decision_and_requires_fresh_review`;
  `tests/workflow.rs::pr_receipt_base_and_repository_come_from_the_forge`;
  `tests/workflow.rs::substantial_acceptance_is_refused_until_reviews_cover_it`;
  `crates/workflow/src/risk.rs::recorded_chair_downgrade_refuses_boundary_paths`;
  `crates/workflow/src/store/concurrency.rs::concurrent_process_writers_on_one_revision_have_one_winner`
- Owner: `crates/workflow/src/receipt.rs`, `crates/workflow/src/store/`

## Sessions

A checkpoint is keyed exactly by project and host session; a session
registered to another project requires an explicit project selection. Each save
keeps the text it replaces as a bounded, hashed revision. Hooks are advisory:
they emit only their own key's checkpoint, and mark a missing or slow
source unavailable, never empty. Autosave never writes the chair's
`checkpoint.md`.
Hook state carries custody-ledger, lane and worktree rows; it has no checkpoint-derived PR rows or PR scan.
- Why: restoring the wrong or a lost checkpoint misdirects an agent, and absent
  evidence must not read as an empty queue.
- Test: `tests/workflow.rs::session_save_refuses_another_registered_project`;
  `crates/workflow/src/session.rs::each_save_keeps_the_replaced_checkpoint_as_a_linked_hashed_revision`;
  `::malformed_missing_wrong_event_and_oversize_hooks_are_advisory_without_fallback`;
  `crates/workflow/src/session/state.rs::unreadable_sections_are_named_not_shown_empty`;
  `tests/hooks_session.rs::autosave_never_writes_the_chair_file_and_output_shows_both_parts`
- Owner: `crates/workflow/src/session.rs`, `crates/workflow/src/session/state.rs`

### Structured summary revisions and inbox coverage

Summary edits share the checkpoint's lock and revision guard. Inbox
reconciliation compares only the matching host, project and chair; anything
observed elsewhere or missed by an incomplete scan is unknown, not stale.
- Why: partial observation must not represent a live request as resolved.
- Test: `crates/workflow/src/session.rs::retained_summary_diff_compares_fields_and_reports_missing`;
  `crates/cli/src/console/overview.rs::compares_only_matching_host_project_and_chair_and_respects_partial_coverage`

### Explicit chairs are discoverable

Only an explicit checkpoint save registers a chair; lanes, autosave and reads
never do, and readers report stale discovery instead of repairing it.
- Why: delivery readers need exact chair identities without guessing ownership.
- Test: `crates/workflow/src/session.rs::session_save_discovery_restart`;
  `::discovery_reports_interrupted_publication_without_read_repair`

## Chair logs

The same operation ID and payload return the original sequence inside the
replay horizon; a different payload under that ID is refused. Lanes cannot
append, listing creates no state, and cost depends on retained data, not
lifetime history.
- Why: retries must not duplicate updates, and long sessions must stay bounded.
- Test: `crates/workflow/src/log.rs::log_operation_replay_pagination_correction_pruning_and_restart`;
  `::replay_store_has_no_lifetime_receipt_cap`;
  `tests/workflow.rs::chair_log_cli_retention_replay_and_lane_refusal`

## Session handoff

A handoff sends a generated prompt, never the transcript, to a peer that has
opted in. The lease moves only after the peer accepts, and a repeated operation
never launches again.
- Why: a handoff must not lose work, launch twice or use an unregistered account.
- Test: `crates/fabric-runtime/src/handoff/session/tests.rs::receive_refuses_until_the_host_opts_in`;
  `::the_peer_takes_the_lease_only_for_a_launch_or_no_launch`
