# Workflow

Owner: `crates/workflow` (coverage, receipts, sessions, writing),
`crates/fabric-runtime/src/handoff/session` (session handoff)
Anchor: `crates/workflow/src/coverage.rs`, `crates/workflow/src/store/tests.rs`,
`crates/workflow/src/session.rs`, `tests/workflow.rs`

Workflow is a small synchronous policy library: a pure review coverage check,
a minimal delivery receipt, bounded session checkpoints and a writing
matcher that runs on JSON rules, with no SQLite, process runner or MCP tool. Pull requests carry their own
evidence. This spec grants no merge,
install or release authority. Paths are relative to `tools/front-door/`.

## Review coverage

### Only independent, exact reviews count

A review passes only for a retained matching verdict and output on the exact
subject, with adequate tier and an independent reviewer. Authors and decision
makers, including the chair, never count, and repeats add nothing.
`approve_with_nits` passes only when every remaining finding is advisory.
Counted reviewers must be at least the highest author tier, and family separation is
measured against all authors.
- Why: self-review or a stale subject proves nothing.
- Test: `crates/workflow/src/coverage.rs::authors_decision_makers_repeats_and_low_tiers_do_not_count`;
  `::a_review_of_another_subject_never_counts`
- Owner: `crates/workflow/src/coverage.rs`

### Required coverage by risk

| Risk | Counted independent passing reviews | Coverage |
|---|---:|---|
| Routine | 0 | Chair plus objective checks |
| Substantial | 1 | Other family if available; otherwise a recorded blocker and a same-family reviewer through a distinct lens |
| Crucial | 2 | Distinct lenses and two other families if available; each unavailable leg has a recorded blocker and an independent same-family replacement |
| Terminal | 2 | Crucial coverage plus an adversarial lens, which may be one of the two; explicit user acceptance |

A blocker needs source references and an expiry; a reason string alone never
waives a leg.
- Why: higher-risk changes need diverse, independent eyes.
- Test: `crates/workflow/src/coverage.rs::crucial_needs_two_distinct_lenses_and_terminal_an_adversarial_one`;
  `::duplicate_blocker_evidence_waives_only_one_cross_family_leg`
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
rechecks derived risk, and a downgrade needs an originating user approval.
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

A checkpoint is bounded UTF-8 Markdown under an explicit project and host
session key, saved atomically with its bytes preserved. Session IDs are 1 to
128 ASCII letters, digits, `.`, `_` and `-`, starting alphanumeric, with no
`..`, so the injected save command is safe to copy. Symlinks are refused.
- Why: restoring the wrong checkpoint misdirects an agent.
- Test: `crates/workflow/src/session.rs::refuses_traversal_and_command_injection_in_host_keys`;
  `::refuses_symlinked_directories_checkpoints_and_locks`
- Owner: `crates/workflow/src/session.rs`

### Session hooks are advisory

`pre-compact`, `compact`, `session-start` and `stop` exit 0 on missing or invalid state with a
diagnostic naming a cause kind, never a path. SessionStart emits only the
checkpoint for its own key, or a missing or stale status. It never emits the
latest file, withholds stale content and caps output at 8 KiB.
- Why: a hook must never block the user or inject another session.
- Test: `crates/workflow/src/session.rs::malformed_missing_wrong_event_and_oversize_hooks_are_advisory_without_fallback`;
  `::native_host_fixtures_select_only_their_key_and_emit_native_json`
- Owner: `crates/workflow/src/session.rs`

### Session state is derived, bounded and never authority

`session-start` and `compact` add a digest of this project's in-flight
lanes, the lanes settled since the chair last saved, and `pr status` for the
pull requests the chair named in the checkpoint. The digest is at most 3 KiB,
read-only and shares the 8 KiB output cap, so the checkpoint excerpt shrinks
first. Lane, PR and worktree facts come from the read commands (`lanes`,
`pr status`, `worktree list`) under one 4 second deadline. A command that is
missing, slow or failing marks its section unavailable and never reads as
empty. Record fields are stripped of markers, backticks and control
characters before they are shown.
- Why: a restarted chair must see real lane state quickly, and absence of
  evidence must not read as an empty queue.
- Test: `crates/workflow/src/session/state.rs::output_is_bounded_and_says_when_it_was`;
  `::unreadable_sections_are_named_not_shown_empty`;
  `::hostile_fields_cannot_forge_markers_or_headings`;
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
`autosave.md`. A checkpoint that holds a block from an earlier release keeps it
as chair text. `stop` never creates a session directory, `autosave.md` alone is
reported as holding only automatically saved state, and lane sessions never
write. A next-page command in the digest is shown only if it survives
rendering byte for byte (within 512 bytes and free of `<`, `>` and backticks);
otherwise a plain note names the cursor.
- Why: automatic state must not erase the authority references the chair wrote
  or hide that the chair has not saved notes; separate files remove the
  ownership question a shared file raises.
- Test: `tests/hooks_session.rs::autosave_never_writes_the_chair_file_and_output_shows_both_parts`;
  `::a_legacy_block_in_the_checkpoint_stays_chair_text`;
  `::pre_compact_without_a_checkpoint_saves_state_and_says_notes_are_missing`;
  `::stop_hook_refreshes_only_an_existing_quiet_checkpoint`;
  `::lane_sessions_report_but_never_write_checkpoint_state`;
  `crates/workflow/src/session/state.rs::a_next_page_command_shows_only_if_it_survives_rendering_byte_for_byte`
- Owner: `crates/workflow/src/session/state.rs`, `crates/workflow/src/session.rs`, `src/hooks.rs`

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
  `::the_peer_takes_the_lease_only_for_a_launch_or_no_launch`
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
- Test: none (review gate); `crates/workflow/src/risk.rs::lowering_risk_requires_matching_current_user_authority`
  covers the one gate workflow holds.
- Owner: the orchestrate and evaluate skills; `crates/workflow/src/`

## Not provided

- Receipt mutation commands, repair and finding sidecars.
- Provider-family maps in workflow, or a tracker replay queue.
- Transcript reinjection and workflow MCP tools.
