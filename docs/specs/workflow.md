# Workflow

Owner: `crates/workflow` (coverage, receipts, sessions, chair logs, writing),
`crates/fabric-runtime/src/handoff/session` (session handoff)
Anchor: `crates/workflow/src/coverage.rs`, `crates/workflow/src/store/tests.rs`,
`crates/workflow/src/session.rs`, `crates/workflow/src/log.rs`, `tests/workflow.rs`

Workflow is a small synchronous library: a pure review coverage check,
a minimal delivery receipt, bounded session checkpoints, a SQLite-backed chair
log and a writing matcher that runs on JSON rules, with no process runner or MCP
tool. Pull requests carry their own evidence. This spec grants no merge,
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

[HARNESS.md](../../HARNESS.md#review-ladder) owns the review ladder. This
spec enforces its counts:

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
  `::refuses_symlinked_directories_checkpoints_and_locks`;
  `tests/workflow.rs::session_save_outside_checkout_requires_project`;
  `::session_save_refuses_conflicting_explicit_project`;
  `::session_save_refuses_another_registered_project`;
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
  `::only_the_newest_revisions_are_kept_and_older_files_are_deleted`;
  `::an_existing_checkpoint_or_an_interrupted_save_is_recorded_not_lost`;
  `::unreadable_history_never_blocks_a_save_and_a_changed_revision_is_refused`;
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

Catalogue composition uses the bounded discovery reader with a shared entry
budget. Every enumerated session-directory entry consumes that budget, even if
its name is invalid or it has no discovery record. Reaching the budget reports
truncation without reading a further entry; an exact fit is conservatively
reported as truncated. The original discovery reader retains its existing limit
and error behaviour for other callers.

**Contract for later slices:** `discovery.json` is advisory. The checkpoint and
revision manifest commit before the separate discovery replacement. Failure of
that replacement returns a successful save with a warning; the next save repairs
the entry. Readers must compare its revision and hash with the manifest and
checkpoint and explicitly report stale metadata before using it.

`workflow session save SESSION` registers the caller's exact Fabric host,
bound project, supplied host session ID and trusted actor as a chair.
Chair identity paths use Fabric's shared inbox derivation: host plus the first
nonempty label from `PROVENANT_AGENT_LABEL`, configured `fabric.agent_label`,
the harness client, then configured `fabric.client_label` (default `agent`).
The client is explicit `--client HARNESS` when supplied. Hook-generated save
commands pass the installing harness as `--client claude` or `--client codex`.
Otherwise, a nonempty `CODEX_THREAD_ID` identifies Codex; `CLAUDECODE=1`,
`CLAUDE_CODE_SESSION_ID` or `CLAUDE_SESSION_ID` identifies Claude.
If both families have markers, no harness is inferred: supply `--client`.
Configuration directory variables do not identify an active harness.
With the same labels and client, the chair actor equals its Fabric inbox actor;
unlabelled Claude and Codex chairs on the same host have different actors.
The same derivation applies to CLI saves, MCP and source checkpoint saves
during `session handoff`. Hooks include the client in
`provenant workflow session save SESSION --file <checkpoint.md> --client HARNESS`.
General CLI Fabric and lane commands retain their configured actor and
`identity.client` (default `agent`); harness markers do not change either.
Handoff validates the binding before peer delivery. Its recovery command
includes the supplied client or `--client HARNESS` when none was supplied.
Lanes (`context.in_lane`) are refused before saving or registering a
chair, including when they request `--rebind`. The workflow
owner stores a per-session index entry in
`workflow/projects/<project>/sessions/<session>/discovery.json` (at most 4 KiB):

| Field | Contract |
|---|---|
| `schema_version` | `1` |
| `host`, `project_id`, `host_session_id` | Exact session key; host/project IDs come from their existing owners |
| `chair_actor` | Canonical Fabric actor; only an explicit `save --rebind` may replace it or the host |
| `created_at`, `last_saved_at` | Canonical RFC3339 UTC with microseconds; first registration and latest save whose discovery was published, including unchanged saves |
| `checkpoint` | The complete current `Revision`: revision, parent, SHA-256, bytes and saved time |
| `rebound_from` | Optional list of the last 8 previous host/actor bindings and `changed_at`, oldest first; preserved by ordinary saves. Longer lists are truncated to the newest 8 on read. Legacy single-hop objects remain readable and become lists on save |

The discovery key is scoped to this host's state directory. Separate host state
directories can contain the same project/session ID without collision. Shared
state directories across hosts are unsupported; peer composition belongs to
slice 16.

The entry denotes role `chair` and evidence source `checkpoint_save`. No
provider, provider session, label, title, parent, route, summary, log, lifecycle,
lane association or interaction channel is inferred. Those absent facts remain
unknown; later slices own their capture. A provider session handle in a worker
attempt is never registration evidence. Autosave and checkpoint reads do not
register chairs. Legacy manifests and bare checkpoints remain readable with no
discovery entry; summaries remain unavailable.

The save holds `checkpoint.lock` across checkpoint replacement, atomic revision
manifest replacement, then atomic discovery replacement, in that order. Each
replacement is atomic; the group is not a transaction. A discovery publication
failure is returned in `warnings` (handoff: `checkpoint_warnings`), with the
checkpoint save still successful. The revision manifest stays unchanged
in shape, so older binaries can read and save it without losing history; an
older writer leaves the discovery entry stale. There is no shared index that
can race another session's writer. If a process stops after publishing the
checkpoint or manifest but before discovery, an earlier entry remains at its
earlier revision/hash. `SessionStore::discovery` compares the
held checkpoint's hash and manifest revision against that entry, returning
`current`, `checkpoint_changed` or `checkpoint_missing`. These are bounded
observations, not a lease or an assertion that the provider is alive. Unreadable
files return an error. A crash on the first save can leave a readable checkpoint
without registration, which remains unknown until an explicit save succeeds.
Unreadable discovery metadata blocks a save before replacing checkpoint or
history. An explicit host/actor mismatch is refused as `IDENTITY`, with an error
instructing callers to repeat `workflow session save <id>` with the same options
and add `--rebind`. Handoff refusals provide a save command with the actual
session ID, explicit client, project and state directory. To recover an older
binding under `agent` or deliberately change labels, run
`provenant workflow session save SESSION --file FILE --client HARNESS --rebind`.
This records the previous identity and change time under the checkpoint lock.
Human and JSON saves report discovery warnings. If publication warns, repeat
the same command, including `--rebind` when used, to commit the new binding.
For unreadable discovery, restore a readable record from backup before retrying;
`--rebind` does not bypass corrupt metadata or a project/session key mismatch.
The next save reconciles revisions and publishes the corresponding entry;
identity must be supplied again when registration was never committed.

`discover_sessions` enumerates only committed explicit entries, in session-key
order, with a 16,384-directory-entry scan ceiling. Exceeding it or encountering
unreadable state returns an error rather than claiming complete coverage.
Discovery, `session show` (all modes) and `session diff` create no files, locks,
directories or project/host identities, and never repair state. These CLI reads
also skip demand-usage recording. Consumers must
check consistency before treating cached checkpoint metadata as current.
`show` and `diff` honour `--project` and `--state-dir`, including missing stores
and unbound projects, without allocating state.

CLI error codes: `IDENTITY` for a binding/key mismatch; `IN_LANE` for a lane's
chair save; `HISTORY` for malformed discovery JSON or invalid revision history;
`IO` for unreadable discovery files (permissions, symlinks, size limits or other
filesystem failures), checkpoint encoding and pre-commit filesystem/deadline
errors; `MISSING` for an unbound project or absent checkpoint/revision; `INVALID`
for invalid session IDs, options or oversized checkpoint input. A post-manifest
discovery publication failure is a warning, not an error code. Discovery library
readers return `SessionError::History`, `DiscoveryIdentity` or `Filesystem`
rather than claiming complete coverage of unreadable state.

- Why: later delivery and catalogue readers need durable exact identities without guessing ownership.
- Test: `crates/workflow/src/session.rs::session_save_discovery_restart`;
  `::discovery_reports_interrupted_publication_without_read_repair`;
  `::discovery_publication_failure_warns_after_manifest_commit_and_next_save_repairs`;
  `tests/workflow.rs::session_save_discovery_restart`;
  `::session_reads_allocate_nothing`;
  `::session_show_honours_project_and_state_dir_without_allocating`;
  `::a_lane_cannot_register_or_overwrite_a_chair`
- Owner: `crates/workflow/src/session/discovery.rs`, `crates/cli/src/workflow.rs`

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
  `::a_legacy_block_in_the_checkpoint_stays_chair_text`;
  `::pre_compact_without_a_checkpoint_saves_state_and_says_notes_are_missing`;
  `::stop_hook_refreshes_only_an_existing_quiet_checkpoint`;
  `::lane_sessions_report_but_never_write_checkpoint_state`;
  `crates/workflow/src/session/state.rs::a_next_page_command_shows_only_if_it_survives_rendering_byte_for_byte`;
  `::the_digest_title_names_the_store_it_was_read_from`;
  `::the_bounded_fallback_lists_the_store_the_digest_came_from`;
  `crates/workflow/src/session.rs::a_damaged_or_unreadable_autosave_never_hides_a_readable_checkpoint`;
  `tests/hooks_session.rs::a_damaged_autosave_leaves_the_chair_notes_visible_at_session_start`;
  `crates/workflow/src/session.rs::the_save_command_carries_a_custom_state_dir_or_refuses_to_show_one`;
  `::the_excerpt_notice_carries_the_state_dir_and_stays_within_the_cap`
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
  `::replay_horizon_expires_receipts_and_survives_clock_and_config_changes`;
  `::replay_store_has_no_lifetime_receipt_cap`;
  `tests/workflow.rs::chair_log_cli_replay_and_pages`;
  `::chair_log_cli_retention_replay_and_lane_refusal`
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
- Merging, syncing or conflict-free replication of checkpoint revisions; the
  history is one writer's bounded chain.

## Pipeline experiments

Projects may opt into named experiments at
`domains.fabric.pipelines.experiment.NAME`. Each experiment names a control,
candidate preset arms and a `hash` or `epsilon` assignment strategy. Experiments
apply only to automatic selection with `--task-id`, because `--preset` cannot
be combined with `--task-id`. The control must be the rule-selected preset, and
each arm must pass the same selection and expansion as an ordinary run. Assignment
is skipped for work without a risk tier and for terminal-tier work. It stops at
expiry, and at the configured spend budget or cost ratio, using outcome rows from
the last 14 days read once per run. Unreadable spend skips the experiment.

Graph context retains the experiment ID, assignment seed, executed arm and, in
shadow mode, the candidate as `shadow_of`. The executed arm is the outcome label
in ordinary work-context telemetry.

Shadow mode assigns the candidate for comparison but expands and runs the
control graph, so candidate work cannot gate acceptance. The shadow graph is
the same as a plain control run, and its outcomes are recorded as control.
`telemetry experiments show ID` uses the offline pipeline outcomes and ranking
inputs; arm comparisons remain advisory and are stratified by risk tier and
task kind. It cannot yet pair shadow runs with their candidate.

## Lane delivery evidence

Lane delivery modes and later `lane deliver` pickup use the run owner's sealed
terminal result and exact Git head. PR bodies are supplied from that result,
and graph PRs combine bounded node result excerpts with their attempt IDs.
A host delivery or worktree cleanup receipt is separate from the chair's
workflow acceptance receipt. Successful push or local integration grants no
additional review, protected-branch merge or release authority.
Delivery modes refused before admission when targeting a protected branch are
`integrate` and direct `push`, including graph nodes, legacy delivery flags and
manifests. The protected branches are the primary checkout branch, each configured
remote's recorded default, or `main` and `master` when no remote default is
recorded. PR mode onto the default branch is allowed, using the default branch
as its base while publishing an unprotected head branch. Replay of an accepted
operation is not refused before admission if branch protection changes later,
though actual delivery still rechecks protection.
A succeeded lane whose delivery outcome is `NoNewCommits` keeps its worktree
on purpose, because cleanup requires a Delivered receipt. The tree is kept so
idle runs or uncommitted edits can be inspected, and is reclaimed later
through gc or disk reclaim.
- Why: lifecycle automation must preserve the distinction between publishing
  evidence and accepting a change under the repository's workflow.
- Test: `crates/run/src/delivery_tests.rs::settled_delivery_refuses_dirty_or_changed_head_and_replays_success`;
  `src/fabric.rs::graph_submission_refuses_a_push_to_main_before_admission`;
  `src/fabric.rs::graph_submission_replays_an_accepted_push_after_protection_changes`;
  `crates/fabric-runtime/src/graph.rs::graph_delivers_nodes_and_opens_one_final_pr_only_after_success`.
- Owner: `crates/run/src/delivery.rs`, `src/fabric.rs`; acceptance remains with `crates/workflow`

## Bounded planner pattern expansion

A plan node may declare an allow-list of patterns with typed parameters and
fixed nodes. A fragment instantiates a listed pattern by name and parameter
values only; it cannot replace the pattern's access, role, source or read
roots. The runtime validates parameters against their schemas and expands the
pattern at the same composite boundary a statically written preset uses, with
the pattern name and effective parameters retained in the expansion. Unlisted
patterns, invalid parameters and widening attempts pause the graph without
admitting children. Expanded nodes count against the graph node limit and the
plan's node limit.
- Why: a plan may grow a pipeline from known-good shapes without letting the
  planner choose authority or exceed host limits.
- Test: `crates/fabric-runtime/src/graph/plan_tests.rs::allowed_pattern_instantiation_records_parameters_and_matches_a_static_composite`;
  `::plan_patterns_refuse_unlisted_invalid_and_widening_instances`;
  `::pattern_expansion_counts_against_the_graph_node_limit`;
  `::pattern_children_obey_plan_node_and_kind_limits`.
- Owner: `crates/fabric-runtime/src/graph/plans.rs`,
  `crates/fabric-runtime/src/graph/pattern_plan.rs`
