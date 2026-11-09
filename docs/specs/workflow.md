# Workflow

Owner: `crates/workflow` (coverage, receipts, sessions, chair logs, writing),
`crates/fabric-runtime/src/handoff/session` (session handoff)
Anchor: `crates/workflow/src/coverage.rs`, `crates/workflow/src/store/tests.rs`,
`tests/workflow.rs`

Workflow is a small synchronous library: a pure review coverage check,
a minimal delivery receipt, bounded session checkpoints, a SQLite-backed chair
log and a writing matcher that runs on JSON rules, with no process runner or MCP
tool. Pull requests carry their own evidence. This spec grants no merge,
install or release authority. Paths are relative to `tools/front-door/`.

## Findings dispositions

A leaf node declaring dispositions selects findings from each dependency and
explicitly selected upstream node. A
synthesis may cite a selected finding by its qualified id, or by a unique bare
id, and inherit its descriptive fields. Each selected upstream finding still
requires exactly one disposition and reason. An unknown identity without the
descriptive fields is skipped with a warning when another valid entry remains;
if none remains, the findings block is invalid.

- Why: reviewers can reconcile findings by identity without repeating their descriptions.
- Test: `crates/fabric-runtime/src/graph/outputs.rs::unknown_identity_warns_when_another_disposition_is_valid`;
  `crates/fabric-runtime/src/graph/graph_synthesis_findings_tests.rs::a_synthesis_must_dispose_of_every_selected_upstream_finding_exactly_once`;
  `crates/fabric-runtime/src/graph/graph_synthesis_findings_tests.rs::identity_only_dispositions_inherit_selected_findings`
- Owner: `crates/fabric-runtime/src/graph/outputs.rs`

## Review coverage

### Test changes always receive the tests lens

Every review panel for a diff that changes a recognised test path or changes a
test declaration in a diff hunk includes `tests`, even when the selected shape
has one slot. Additions sit outside the slot count and are deduplicated when
`tests` already occupies a slot. A diff with neither signal keeps its normal
panel. Repository routing config can replace or extend
`review.lenses.by_paths`; an empty table disables the bundled path rules.
- Why: small test changes need test-design review as well as the shape's base lens.
- Test: `crates/routing/src/review.rs::selected_panels_keep_change_driven_lenses`
- Owner: `crates/routing/src/review.rs`

### Only independent, exact reviews count

A review passes only for a retained matching verdict and output on the exact
subject, with adequate tier and an independent reviewer. Authors and decision
makers, including the chair, never count, and repeats add nothing.
`ReviewVerdict` uses the [review wire vocabulary](fabric.md#typed-node-outputs).
Both `approve` and `approve_with_nits` require empty `blocking_findings` to pass;
there is no advisory-findings list. `request_changes` requires at least one
blocking finding and blocks acceptance on the exact subject. `inconclusive`
never counts towards coverage and never blocks acceptance, regardless of its
findings; it emits an `inconclusive_review` diagnostic asking for a fresh review.
Insufficient passing coverage still leaves acceptance incomplete.
Counted reviewers must be at least the highest author tier, and family separation is
measured against all authors.
- Why: self-review or a stale subject proves nothing.
- Test: `crates/workflow/src/coverage.rs::authors_decision_makers_repeats_and_low_tiers_do_not_count`;
  `crates/workflow/src/coverage.rs::a_review_of_another_subject_never_counts`
- Owner: `crates/workflow/src/coverage.rs`

### Required coverage by risk

[HARNESS.md](../../HARNESS.md#review-ladder) owns the review ladder. This
spec describes the current checker counts, which are below the policy floors
for routine, substantial and terminal work:

| Risk | Counted independent passing reviews | Enforced beyond the count |
|---|---:|---|
| Routine | 0 | None |
| Substantial | 1 | Cross-family leg or a recorded blocker |
| Crucial | 2 | Distinct lenses; each cross-family leg or a recorded blocker |
| Terminal | 2 | Adversarial lens, which may be one of the two |

A blocker needs source references and an expiry; a reason string alone never
waives a leg.
- Why: higher-risk changes need diverse, independent eyes.
- Test: `crates/workflow/src/coverage.rs::crucial_needs_two_distinct_lenses_and_terminal_an_adversarial_one`;
  `crates/workflow/src/coverage.rs::duplicate_blocker_evidence_waives_only_one_cross_family_leg`
- Owner: `crates/workflow/src/coverage.rs`, `crates/workflow/src/risk.rs`

### Provenance

| Provenance | Author tier | Same-family review | Cross-family review |
|---|---|---|---|
| `Observed` | Counts when verified from the attempt or host record | Counts | Counts only with providers-verified family lineage |
| `ChairAttested` | Counts, with the native host parameter, tier and evidence reference recorded | Counts, with an independent actor and lens | Never counts |
| `Unknown` | Blocks substantial-or-higher coverage | Findings only | Never counts |

Attestation never promotes itself to observation, and an unknown author
family leaves separation unproven.
- Why: only observed evidence proves a different family reviewed.
- Test: `crates/workflow/src/coverage.rs::attested_reviewer_never_fills_a_cross_family_leg_without_a_blocker`;
  `crates/workflow/src/bind.rs::observed_reviewer_family_is_verified_and_attested_is_not`
- Owner: `crates/workflow/src/review.rs`, `crates/workflow/src/bind.rs`

### Binding at the edge

The reviewer identity is the dispatched attempt's own execution identity,
never the submitting chair, and the family comes from the catalogue entry for
the observed model (unknown if absent). A PR review's attempt head must equal
the subject head; a read-only lane's head is the HEAD the host read at
dispatch, never lane output. The CLI records, but does not verify, the
chair-supplied `LegBlocker` and `user_approval` references.
- Why: a reviewer's output must not assert its own identity.
- Test: `crates/workflow/src/bind.rs::failed_headless_wrong_head_and_foreign_delivery_attempts_are_refused`;
  `crates/cli/src/workflow.rs::family_comes_from_the_catalogue_and_is_unknown_otherwise`
- Owner: `crates/workflow/src/bind.rs`

## Receipts

### A receipt records a decision, not a merge

A receipt records the chair's decision and is not a merge certificate. A
changed subject resets the decision and needs fresh review. Each save
rechecks derived risk against the effective policy and recorded changed paths.
A downgrade uses a chair record with actor, evidence and a nonblank reason of
at most 4096 bytes, stored alongside the path snapshot. Routing compiles the
matched boundary floors from effective configuration into workflow policy at the edge.
PR subjects retain their repository ID and bind the head OID. For a PR save
below terminal, the edge reuses recorded paths when the stored head is unchanged.
Otherwise it resolves the PR from the checkout, verifies its head, and computes
paths from the actual base/head merge base, including both names of renames.
If the diff cannot be read, a downgrade refuses with the reason; an ordinary
save warns, keeps the chosen tier and records no paths. Terminal saves need no
diff. Review reports never query GitHub: they validate recorded paths against
the effective policy. A downgraded receipt without recorded paths must be
re-saved with `workflow receipt save`. Non-PR
subjects have no diffable commits, so downgrades are refused without changing
ordinary tier selection. Any path matching an effective crucial or terminal floor
refuses the downgrade and names the path; owners may remove or lower bundled
path floors through configuration. Auth/security and irreversible external-effect floors remain
binding, including with owner approval. Owner decision approvals remain separate.
Accepting refuses `REVIEW_INCOMPLETE` unless coverage passes.
- Why: acceptance must track the exact subject reviewed.
- Test: `crates/workflow/src/store/tests.rs::changing_subject_resets_decision_and_requires_fresh_review`;
  `tests/workflow.rs::substantial_acceptance_is_refused_until_reviews_cover_it`
- Owner: `crates/workflow/src/receipt.rs`

### Revisioned atomic saves

A save replaces the whole receipt at an expected revision under a stable
lock inode, never unlinked: recheck, validate, write and sync a temporary file,
rename it, sync the directory. Two saves at one revision give one winner, a
crash leaves a complete file, and a corrupt receipt fails closed. No lock is
held during probes, provider waits or GitHub reads.
- Why: concurrent chairs must not lose or interleave decisions.
- Test: `crates/workflow/src/store/concurrency.rs::concurrent_process_writers_on_one_revision_have_one_winner`;
  `crates/workflow/src/store/tests.rs::atomic_write_failures_through_save_preserve_complete_bytes_and_redact_errors`
- Owner: `crates/workflow/src/store/`

## Sessions

### Checkpoints are keyed exactly

A checkpoint is UTF-8 Markdown, bounded to 256 KiB, under an explicit project and host
session key, saved atomically with its bytes preserved. Session IDs are 1 to
128 ASCII letters, digits, `.`, `_` and `-`, starting alphanumeric, with no
`..`, so the injected save command is safe to copy. Symlinks are refused.
CLI saves resolve the Git project from the working directory or `--project PATH`.
A non-checkout directory needs the explicit flag; it never becomes a new
project implicitly. When both paths resolve, their Git common directories must
match, including across linked worktrees. Without an explicit flag, a session
already registered on this host in another project refuses with that project's
identity and the `--project` recovery command. Explicit selection permits an
intentional session ID reuse. Successful saves name the selected project ID.
- Why: restoring the wrong checkpoint misdirects an agent.
- Test: `crates/workflow/src/session.rs::refuses_traversal_and_command_injection_in_host_keys`;
  `crates/workflow/src/session.rs::refuses_symlinked_directories_checkpoints_and_locks`;
  `tests/workflow.rs::session_save_outside_checkout_requires_project`;
  `tests/workflow.rs::session_save_refuses_conflicting_explicit_project`;
  `tests/workflow.rs::session_save_refuses_another_registered_project`;
  `tests/workflow.rs::long_checkpoints_save_up_to_the_documented_bound`
- Owner: `crates/workflow/src/session.rs`, `crates/cli/src/workflow.rs`, `src/roots.rs`

### Earlier checkpoints are bounded revisions

A save keeps the checkpoint it replaces as numbered revision `N`, in
`rev-N.md` beside `checkpoint.md`, and records every retained revision with its
parent and a SHA-256 in `revisions.json`, all under the save's existing lock.
The order is: store the old text, publish the new checkpoint, publish the
history, then delete revisions beyond `retention.session_revisions` (default 8,
1 to 64, current included), whether the save comes from `session save` or from
`session handoff`. A save that stops part-way leaves a complete
checkpoint, and the next one records the revision it finds; unreadable revision
history never blocks a save. Identical text adds no revision. Each revision is held to
the 256 KiB checkpoint bound. `session show --rev N` and `session diff N` print
a retained revision, or its unified line diff against the current one, as plain
text. Lines compare with their terminators, so text that differs only in its final
newline shows a difference, marked `\ No newline at end of file` as `diff -u`
marks it. A revision whose text fails its hash is refused with `HISTORY`.
- Why: a bad save or a compaction must not destroy the only copy of earlier
  state, and the history must stay bounded.
- Test: `crates/workflow/src/session.rs::each_save_keeps_the_replaced_checkpoint_as_a_linked_hashed_revision`;
  `crates/workflow/src/session.rs::only_the_newest_revisions_are_kept_and_older_files_are_deleted`;
  `crates/workflow/src/session.rs::an_existing_checkpoint_or_an_interrupted_save_is_recorded_not_lost`;
  `crates/workflow/src/session/diff.rs::a_newline_only_change_is_a_difference_with_a_note`;
  `src/handoff.rs::a_handoff_checkpoint_keeps_the_configured_number_of_revisions`;
  `tests/workflow.rs::session_checkpoints_keep_bounded_revisions_and_show_what_changed`
- Owner: `crates/workflow/src/session.rs`, `crates/workflow/src/session/diff.rs`

### Structured summary revisions and inbox coverage

Summary field edits retain the checkpoint body and use the same save lock and
revision guard. A bounded comparison of retained summary revisions reports
unavailable history, scalar changes and added or removed request IDs. Inbox
reconciliation compares the local host, project and chair; references observed
elsewhere or unaccounted for by an incomplete scan are unknown, not stale.
Viewer comparisons use the viewer's rendered revision as their baseline.
The CLI validates the session and revision through Workflow before writing to
Fabric; the two owners do not share a transaction. The caller acknowledges the
revision it rendered, and missing retained history remains unavailable.
- Why: concurrent saves and incomplete or differently scoped observations must
  not overwrite chair work or represent a live request as resolved.
- Test: `crates/workflow/src/session.rs::retained_summary_diff_compares_fields_and_reports_missing`;
  `crates/cli/src/console/overview.rs::compares_only_matching_host_project_and_chair_and_respects_partial_coverage`;
  `tests/inbox.rs::viewer_cli_isolation_and_render_race`
- Owner: `crates/workflow/src/session/summary.rs`, `crates/cli/src/console/overview.rs`

### Explicit chairs are discoverable

An explicit checkpoint save registers the exact Fabric host, bound project,
host session ID and trusted actor as a chair. The shared inbox derivation owns
actor identity; explicit clients disambiguate conflicting harness markers.
A worker's provider session never establishes a chair. Lanes cannot register
or overwrite chairs, including with rebind. Autosave and reads never register.

Checkpoint, revision manifest and discovery publish in that order under the
save lock. Each replacement is atomic; the group is not a transaction.
Discovery publication failure warns after a successful checkpoint save, and
the next save reconciles it. Readers compare discovery revision/hash with the
manifest and checkpoint and report stale or missing evidence. They never infer
liveness from discovery or repair it during a read. Malformed discovery blocks
a save before checkpoint replacement.

Changing an explicit host/actor binding requires rebind and retains bounded
prior-binding evidence. Rebind cannot bypass corrupt metadata or key mismatch.
An interrupted first publication may leave a checkpoint without registration;
only another explicit save can register it. Separate host-state roots are
independent; a shared state directory across hosts is unsupported.

Discovery scans consume a shared entry budget, including invalid and
unregistered names. Exhaustion is explicit, never complete coverage. Reads
honour the selected project and state directory without allocating state,
locks or identities. The
[discovery owner](../../tools/front-door/crates/workflow/src/session/discovery.rs)
owns the record schema and consistency states; the
[CLI owner](../../tools/front-door/crates/cli/src/workflow.rs) owns recovery commands
and error mapping.

- Why: later delivery and catalogue readers need durable exact identities without guessing ownership.
- Test: `crates/workflow/src/session.rs::session_save_discovery_restart`;
  `crates/workflow/src/session.rs::discovery_reports_interrupted_publication_without_read_repair`;
  `crates/workflow/src/session.rs::discovery_publication_failure_warns_after_manifest_commit_and_next_save_repairs`;
  `tests/workflow.rs::session_save_discovery_restart`
- Owner: `crates/workflow/src/session/discovery.rs`, `crates/cli/src/workflow.rs`

### Session hooks are advisory

`pre-compact`, `compact`, `session-start` and `stop` exit 0 on missing or invalid state with a
diagnostic naming a cause kind, never a path. SessionStart emits only the
checkpoint for its own key, or a missing or stale status. It never emits the
latest file, withholds stale content and caps output at 8 KiB.
- Why: a hook must never block the user or inject another session.
- Test: `crates/workflow/src/session.rs::malformed_missing_wrong_event_and_oversize_hooks_are_advisory_without_fallback`;
  `crates/workflow/src/session.rs::native_host_fixtures_select_only_their_key_and_emit_native_json`
- Owner: `crates/workflow/src/session.rs`

### Session state is derived, bounded and never authority

`session-start` and `compact` add a digest of this project's in-flight
lanes, the lanes settled since the chair last saved, and `pr status` for the
pull requests the chair named in the checkpoint. The digest is at most 3 KiB,
read-only and shares the 8 KiB output cap, so the checkpoint excerpt shrinks
first. Lane, PR and worktree facts come from the read commands (`status`,
`pr status`, `worktree list`) under one 4 second deadline. A command that is
missing, slow or failing marks its section unavailable and never reads as
empty. Record fields are stripped of markers, backticks and control
characters before they are shown.
- Why: a restarted chair must see real lane state quickly, and absence of
  evidence must not read as an empty queue.
- Test: `crates/workflow/src/session/state.rs::output_is_bounded_and_says_when_it_was`;
  `crates/workflow/src/session/state.rs::unreadable_sections_are_named_not_shown_empty`;
  `crates/workflow/src/session/state.rs::hostile_fields_cannot_forge_markers_or_headings`;
  `tests/hooks_session.rs::restart_hook_reports_in_flight_lanes_and_prs_named_by_the_chair`
- Owner: `crates/workflow/src/session/state.rs`, `src/hook_state.rs`, `src/hooks.rs`

### Autosave never writes the chair's checkpoint

`pre-compact` saves, and `stop` refreshes after ten quiet minutes (neither the
checkpoint nor the last autosave changed), the derived lane, PR and worktree
state to `autosave.md`, a file beside `checkpoint.md` that only hooks write.
No autosave path opens `checkpoint.md` for writing, so the chair's text, its
newlines and its modification time (the "last chair save" baseline) are never
touched, and a copy of saved state in the chair's notes is never replaced.
`session-start` and `compact` render the chair's checkpoint followed by
`autosave.md`. The two are read independently: a damaged or unreadable
`autosave.md` costs one diagnostic line (`Autosave unavailable: <cause>`) and
never hides a readable checkpoint. A checkpoint that holds a block from an earlier release keeps it
as chair text. `stop` never creates a session directory, `autosave.md` alone is
reported as holding only automatically saved state, and lane sessions never
write. A next-page command in the digest is shown only if it survives
rendering byte for byte (within 512 bytes and free of `<`, `>` and backticks);
otherwise a plain note names the cursor. Every other command a hook prints
(the digest heading, the bounded-output marker, the `pr status` hint, the `Save:` line, the `session show`
pointer) repeats the hook's non-default `--state-dir` on the same terms, so
following it reaches the store the hook used; one that cannot is not printed.
- Why: automatic state must not erase the authority references the chair wrote
  or hide that the chair has not saved notes; separate files remove the
  ownership question a shared file raises.
- Test: `tests/hooks_session.rs::autosave_never_writes_the_chair_file_and_output_shows_both_parts`;
  `tests/hooks_session.rs::pre_compact_without_a_checkpoint_saves_state_and_says_notes_are_missing`;
  `tests/hooks_session.rs::stop_hook_refreshes_only_an_existing_quiet_checkpoint`;
  `crates/workflow/src/session/state.rs::a_next_page_command_shows_only_if_it_survives_rendering_byte_for_byte`;
  `crates/workflow/src/session.rs::a_damaged_or_unreadable_autosave_never_hides_a_readable_checkpoint`
- Owner: `crates/workflow/src/session/state.rs`, `crates/workflow/src/session.rs`, `src/hooks.rs`

## Chair logs

Chair updates are scoped to a project and session. Each operation has an opaque
operation ID and caller-supplied `issued_at` time, fixed when it is created and
reused on retry. While the operation remains inside the configured horizon
and the persisted expiry boundary, the same ID and payload return the original
sequence, including after payload pruning; a different payload with that ID must
be refused. Appends older than the replay horizon
return an explicit expired result, and timestamps more than five minutes in the
future are refused. Receipts expire after the replay horizon; no lifetime
operation limit may prevent appends. Persisted expiry boundaries must not move
backwards when the clock or configuration changes.

Entries are returned in sequence order with bounded pagination. Retention
removes oldest payloads under the configured age, count and byte limits.
Listing applies these limits before pagination without changing storage, and
full GC applies them to idle sessions. A correction is a new entry referencing an earlier sequence, which may have been
pruned. Lanes must not append chair updates. Listing must not create state or
require write access. The cost of append and list must depend on retained data
and the replay horizon, not the session's lifetime history.
- Why: retries must not duplicate updates, and inactive or long-lived sessions
  must remain bounded without preventing read-only use.
- Test: `crates/workflow/src/log.rs::log_operation_replay_pagination_correction_pruning_and_restart`;
  `crates/workflow/src/log.rs::replay_horizon_expires_receipts_and_survives_clock_and_config_changes`;
  `crates/workflow/src/log.rs::replay_store_has_no_lifetime_receipt_cap`;
  `tests/workflow.rs::chair_log_cli_replay_and_pages`;
  `tests/workflow.rs::chair_log_cli_retention_replay_and_lane_refusal`
- Owner: `crates/workflow/src/log.rs`, `src/chair.rs`

## Session handoff

`workflow session handoff` sends a generated prompt, never the transcript,
to a fresh Claude session on a peer in one `session_handoff` action. Source
worktrees must be clean and pushed unless `--wip` (which commits and pushes tracked changes first) or
`--allow-dirty`. The target must opt in, check the account against its own
registrations and recreate branches through its own worktree owner. The owner
lease moves
only after the peer accepts, and a repeated operation never launches again.
- Why: a handoff must not lose work, launch twice or use an unregistered
  account.
- Test: `crates/fabric-runtime/src/handoff/session/tests.rs::receive_refuses_until_the_host_opts_in`;
  `crates/fabric-runtime/src/handoff/session/tests.rs::the_peer_takes_the_lease_only_for_a_launch_or_no_launch`
- Owner: `crates/fabric-runtime/src/handoff/session/`, `crates/fabric-runtime/src/handoff.rs`

## Writing checks

`check writing` compiles bounded JSON rules once, excludes code fences,
inline code and declared LaTeX spans, and reports deterministic rule IDs with
Unicode positions; unsupported patterns and oversized rule sets are refused.
- Why: a prose lint must not report inside code or crash on hostile rules.
- Test: `crates/workflow/src/writing/tests.rs::inline_code_cannot_hide_following_lines_or_open_a_fence`;
  `tests/checks.rs::writing_checks_use_typed_findings_and_input_exits`
- Owner: `crates/workflow/src/writing/`

## Route and topology evidence boundary

Dispatch and batch receipts own the actual route, provider, model and attempt
facts; delivery may reference them. Fabric MCP may start dispatch but owns no
lifecycle acceptance. Assurance labels infer no coverage, and route
comparison is task-local and opt-in, with no learned router, leaderboard or
automatic promotion. An issue or receipt records scope or evidence. It is
not approval: release, promotion, activation, sharing, provider login, live
installation, push and destructive action stay gated by current user
authority.
- Why: evidence must never be mistaken for permission.
- Test: none (review gate for external-action authority);
  `crates/workflow/src/risk.rs::recorded_chair_downgrade_lowers_nonboundary_risk`
  and `crates/workflow/src/risk.rs::recorded_chair_downgrade_refuses_boundary_paths`
  cover risk downgrades only.
- Owner: the orchestrate and evaluate skills; `crates/workflow/src/`

## Not provided

- Receipt mutation commands, repair and finding sidecars.
- Provider-family maps in workflow, or a tracker replay queue.
- Transcript reinjection and workflow MCP tools.
- Merging, syncing or conflict-free replication of checkpoint revisions; the
  history is one writer's bounded chain.

## Lane delivery evidence

[Run](run.md#delivery-modes-and-settled-pickup) owns delivery modes, protected
publication targets, replay and host pickup. Host delivery and worktree cleanup
receipts remain separate from workflow acceptance. Publishing or integrating a
head grants no additional review, protected-branch merge or release authority.

## Bounded planner pattern expansion

[Fabric](fabric.md#plan-expansion-and-honest-stop-continuation) owns planner
expansion, parameter validation, authority bounds and writer ordering. Planner
output is evidence to validate, never permission to widen an admitted graph.

### Template planner front door

`provenant run --use plan` starts the planning template. Planners return bounded
node fragments using the same leaf and composite templates as static graphs.
Their catalogue and authority bound every expansion; a fragment cannot widen
access, budgets or delivery authority. `provenant run --explain` inspects the
compiled graph without starting work.

Template composition and planner constraints are specified in
[ADR 0039](../adr/0039-graph-of-templates.md). Review panels resolve at launch;
`provenant run --use review` starts that template. The generated
[CLI reference](../reference/cli-all.md) owns current flags.
